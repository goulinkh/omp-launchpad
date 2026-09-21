import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent/extensibility/extensions"
import { existsSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const PLUGIN_DIR = dirname(fileURLToPath(import.meta.url))
const MANIFEST_PATH = join(PLUGIN_DIR, "Cargo.toml")
type BridgeRequest = { op: string } & Record<string, unknown>
interface ReadParameters extends Record<string, unknown> {
  path: string
  i?: string
}


interface BridgeSuccess {
  ok: true
  text: string
  source_url?: string | null
  details?: Record<string, unknown>
}

interface BridgeFailure {
  ok: false
  error: string
}

type BridgePayload = BridgeSuccess | BridgeFailure

interface BridgeToolResult {
  content: [{ type: "text"; text: string }]
  sourceUrl: string | undefined
  details: Record<string, unknown> & {
    changed: boolean
    provider: "launchpad"
  }
}


export default function launchpadExtension(pi: ExtensionAPI) {
  const z = pi.zod

  pi.setLabel("Launchpad")

  pi.registerTool({
    name: "read",
    label: "Read",
    description:
      "Read files, directories, archives, databases, web URLs, OMP internal resources, and Launchpad resources. " +
      "Launchpad forms: lp://bugs/<id>, lp://<project>, lp://~owner/project/+git/repo, and " +
      "lp://~owner/project/+git/repo/+merge/<id>[/diff]. Add ?comments=0 to omit comments.",
    parameters: z.object({
      path: z.string(),
      i: z.string().optional(),
    }),
    loadMode: "essential",
    approval: "read",
    async execute(_toolCallId, params, signal, onUpdate, ctx) {
      const readParameters = params as ReadParameters
      if (!isLaunchpadUrl(readParameters.path)) {
        if (!ctx.invokeTool) {
          throw new Error("The native read tool is unavailable for delegation")
        }
        return ctx.invokeTool(readParameters, {
          ...(signal ? { signal } : {}),
          ...(onUpdate ? { onUpdate } : {}),
        })
      }
      return bridgeResult(
        await runBridge({ op: "resource_view", target: readParameters.path }, signal, ctx.cwd)
      )
    },
  })

  const sharedParameters = {
    target: z.string().optional(),
    repository: z.string().optional(),
    target_repository: z.string().optional(),
    path: z.string().optional(),
    branch: z.string().optional(),
    query: z.string().optional(),
    status: z.union([z.string(), z.array(z.string())]).optional(),
    importance: z.union([z.string(), z.array(z.string())]).optional(),
    tags: z.array(z.string()).optional(),
    limit: z.number().optional(),
    title: z.string().optional(),
    description: z.string().optional(),
    information_type: z.string().optional(),
    source_ref: z.string().optional(),
    target_ref: z.string().optional(),
    commit_message: z.string().optional(),
    needs_review: z.boolean().optional(),
    body: z.string().optional(),
    subject: z.string().optional(),
    vote: z
      .enum(["Approve", "Needs Fixing", "Needs Information", "Abstain", "Disapprove", "Needs Resubmitting"])
      .optional(),
    directory: z.string().optional(),
    force_with_lease: z.boolean().optional(),
    i: z.string().optional(),
  }

  pi.registerTool({
    name: "launchpad",
    label: "Launchpad",
    description:
      "Read Launchpad through lpcli. Supports repository and resource views, repository file reads, and bug or " +
      "merge-proposal search. Prefer read with lp:// URLs for individual bugs and merge proposals.",
    parameters: z.object({
      op: z.enum(["resource_view", "repo_view", "file_read", "search_bugs", "search_merge_proposals"]),
      ...sharedParameters,
    }),
    approval: "read",
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      const request = params as BridgeRequest
      return bridgeResult(await runBridge(request, signal, ctx.cwd))
    },
  })

  pi.registerTool({
    name: "launchpad_write",
    label: "Launchpad Write",
    description:
      "Mutate Launchpad or local Git state: create bugs or merge proposals, add bug comments or merge-proposal " +
      "reviews, change merge-proposal status, check out a proposal, or push its checked-out branch.",
    parameters: z.object({
      op: z.enum([
        "bug_create",
        "merge_proposal_create",
        "comment",
        "set_merge_proposal_status",
        "merge_proposal_checkout",
        "merge_proposal_push",
      ]),
      ...sharedParameters,
    }),
    approval: "exec",
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      const request = params as BridgeRequest
      return bridgeResult(await runBridge(request, signal, ctx.cwd), true)
    },
  })
}

function isLaunchpadUrl(path: unknown): path is string {
  return typeof path === "string" && path.startsWith("lp://")
}

async function runBridge(
  request: BridgeRequest,
  signal: AbortSignal | undefined,
  cwd: string
): Promise<BridgeSuccess> {
  const command = bridgeCommand()
  const child = Bun.spawn({
    cmd: command,
    cwd,
    env: {
      ...process.env,
      CARGO_TERM_COLOR: "never",
    },
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
    ...(signal ? { signal } : {}),
  })
  child.stdin.write(JSON.stringify(request))
  child.stdin.end()
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ])
  if (exitCode !== 0) {
    throw new Error(stderr.trim() || stdout.trim() || `Rust bridge exited with status ${exitCode}`)
  }
  let payload: unknown
  try {
    payload = JSON.parse(stdout)
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error)
    throw new Error(`Rust bridge returned invalid JSON: ${message}${stderr.trim() ? `\n${stderr.trim()}` : ""}`)
  }
  if (!isBridgePayload(payload)) {
    throw new Error("Rust bridge returned an invalid response")
  }
  if (!payload.ok) {
    throw new Error(payload.error || "Launchpad operation failed")
  }
  return payload
}

function bridgeCommand(): string[] {
  if (process.env.OMP_LAUNCHPAD_BINARY) {
    return [process.env.OMP_LAUNCHPAD_BINARY]
  }
  const packagedBinary = join(
    PLUGIN_DIR,
    "bin",
    `omp-launchpad-${process.platform}-${process.arch}${process.platform === "win32" ? ".exe" : ""}`
  )
  if (existsSync(packagedBinary)) {
    return [packagedBinary]
  }
  const cargo = Bun.which("cargo")
  if (!cargo) {
    throw new Error("cargo is not installed. Install Rust 1.88 or newer and ensure cargo is on PATH.")
  }
  return [cargo, "run", "--quiet", "--release", "--manifest-path", MANIFEST_PATH, "--"]
}

function bridgeResult(payload: BridgeSuccess, changed = false): BridgeToolResult {
  return {
    content: [{ type: "text", text: payload.text }],
    sourceUrl: payload.source_url || undefined,
    details: {
      ...(payload.details || {}),
      changed,
      provider: "launchpad",
    },
  }
}

function isBridgePayload(payload: unknown): payload is BridgePayload {
  if (typeof payload !== "object" || payload === null || !("ok" in payload)) {
    return false
  }
  if (payload.ok === false) {
    return "error" in payload && typeof payload.error === "string"
  }
  if (payload.ok !== true || !("text" in payload) || typeof payload.text !== "string") {
    return false
  }
  if (
    "source_url" in payload &&
    payload.source_url !== null &&
    payload.source_url !== undefined &&
    typeof payload.source_url !== "string"
  ) {
    return false
  }
  return !(
    "details" in payload &&
    payload.details !== undefined &&
    (typeof payload.details !== "object" || payload.details === null || Array.isArray(payload.details))
  )
}
