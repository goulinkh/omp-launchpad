import type {
  ExtensionAPI,
  ExtensionCommandContext,
} from "@oh-my-pi/pi-coding-agent/extensibility/extensions"
import { existsSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const PLUGIN_DIR = dirname(fileURLToPath(import.meta.url))
const MANIFEST_PATH = join(PLUGIN_DIR, "Cargo.toml")
const LOGIN_PROMPT = "After authorising, press Enter to continue..."
const LOGIN_URL = /https:\/\/launchpad\.net\/\+authorize-token\?oauth_token=[^\s]+/
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
  pi.registerCommand("launchpad", {
    description: "Manage Launchpad authentication (login, logout, or status)",
    getArgumentCompletions(argumentPrefix) {
      if (argumentPrefix.includes(" ")) return null
      const prefix = argumentPrefix.trim().toLowerCase()
      const commands = [
        { label: "login", value: "login", description: "Authenticate with Launchpad" },
        { label: "logout", value: "logout", description: "Remove stored Launchpad credentials" },
        { label: "status", value: "status", description: "Check connectivity and authentication" },
      ]
      const completions = commands.filter(command => command.value.startsWith(prefix))
      return completions.length > 0 ? completions : null
    },
    async handler(args, ctx) {
      const command = args.trim().toLowerCase()
      try {
        if (command === "login") {
          await login(pi, ctx)
          return
        }
        if (command === "logout" || command === "status") {
          const output = await runCommand(command, ctx.cwd)
          ctx.ui.notify(output, "info")
          return
        }
        ctx.ui.notify("Usage: /launchpad login | logout | status", "warning")
      } catch (error) {
        ctx.ui.notify(error instanceof Error ? error.message : String(error), "error")
      }
    },
  })


  pi.registerTool({
    name: "read",
    label: "Read",
    description:
      "Read files, directories, archives, databases, web URLs, OMP internal resources, and Launchpad resources. " +
      "Launchpad forms: lp://bugs/<id>, lp://<project>, lp://~owner/project/+git/repo, and " +
      "lp://~owner/project/+git/repo/+merge/<id>[/diff[/<preview-diff-id>]]. Add ?comments=0 to omit comments.",
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

  const withIntent = { i: z.string().optional() }
  const oneOrManyStrings = z.union([z.string(), z.array(z.string())])
  const positiveInteger = z.number().int().positive()
  const diffSide = z.enum(["original", "modified"])
  const reviewVote = z.enum([
    "Approve",
    "Needs Fixing",
    "Needs Information",
    "Abstain",
    "Disapprove",
    "Needs Resubmitting",
  ])

  pi.registerTool({
    name: "launchpad",
    label: "Launchpad",
    description:
      "Read Launchpad through lpcli. Supports repository and resource views, repository file reads, searches, " +
      "preview-diff history and selection, published inline comments, review drafts, and file-line mapping. " +
      "Prefer read with lp:// URLs for individual bugs, merge proposals, and diff text.",
    parameters: z.union([
      z.object({
        op: z.literal("resource_view"),
        target: z.string(),
        preview_diff_id: positiveInteger.optional(),
        ...withIntent,
      }),
      z.object({ op: z.literal("repo_view"), repository: z.string(), ...withIntent }),
      z.object({
        op: z.literal("file_read"),
        repository: z.string(),
        path: z.string(),
        branch: z.string().optional(),
        ...withIntent,
      }),
      z.object({
        op: z.literal("search_bugs"),
        target: z.string(),
        query: z.string().optional(),
        status: oneOrManyStrings.optional(),
        importance: oneOrManyStrings.optional(),
        tags: z.array(z.string()).optional(),
        limit: positiveInteger.optional(),
        ...withIntent,
      }),
      z.object({
        op: z.literal("search_merge_proposals"),
        repository: z.string(),
        status: oneOrManyStrings.optional(),
        limit: positiveInteger.optional(),
        ...withIntent,
      }),
      z.object({ op: z.literal("preview_diffs"), target: z.string(), ...withIntent }),
      z.object({
        op: z.literal("inline_comments"),
        target: z.string(),
        preview_diff_id: positiveInteger,
        ...withIntent,
      }),
      z.object({
        op: z.literal("review_drafts"),
        target: z.string(),
        preview_diff_id: positiveInteger,
        ...withIntent,
      }),
      z.object({
        op: z.literal("diff_line_map"),
        target: z.string(),
        preview_diff_id: positiveInteger,
        path: z.string(),
        file_line: positiveInteger,
        side: diffSide,
        ...withIntent,
      }),
    ]),
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
      "Mutate Launchpad or local Git state: create bugs or merge proposals, add comments, update inline review " +
      "drafts, submit reviews, change proposal status, check out a proposal, or push its checked-out branch.",
    parameters: z.union([
      z.object({
        op: z.literal("bug_create"),
        target: z.string(),
        title: z.string(),
        description: z.string(),
        information_type: z.string().optional(),
        tags: z.array(z.string()).optional(),
        ...withIntent,
      }),
      z.object({
        op: z.literal("merge_proposal_create"),
        repository: z.string(),
        target_repository: z.string().optional(),
        source_ref: z.string(),
        target_ref: z.string(),
        description: z.string().optional(),
        commit_message: z.string().optional(),
        needs_review: z.boolean().optional(),
        ...withIntent,
      }),
      z.object({
        op: z.literal("comment"),
        target: z.string(),
        body: z.string(),
        subject: z.string().optional(),
        vote: reviewVote.optional(),
        ...withIntent,
      }),
      z.object({
        op: z.literal("review_draft_update"),
        target: z.string(),
        preview_diff_id: positiveInteger,
        path: z.string(),
        file_line: positiveInteger,
        side: diffSide,
        body: z.string().optional(),
        ...withIntent,
      }),
      z.object({
        op: z.literal("review_submit"),
        target: z.string(),
        preview_diff_id: positiveInteger,
        body: z.string().optional(),
        vote: reviewVote.optional(),
        ...withIntent,
      }),
      z.object({
        op: z.literal("set_merge_proposal_status"),
        target: z.string(),
        status: z.string(),
        ...withIntent,
      }),
      z.object({
        op: z.literal("merge_proposal_checkout"),
        target: z.string(),
        directory: z.string().optional(),
        ...withIntent,
      }),
      z.object({
        op: z.literal("merge_proposal_push"),
        directory: z.string().optional(),
        force_with_lease: z.boolean().optional(),
        ...withIntent,
      }),
    ]),
    approval: "exec",
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      const request = params as BridgeRequest
      return bridgeResult(await runBridge(request, signal, ctx.cwd), true)
    },
  })
}
async function login(pi: ExtensionAPI, ctx: ExtensionCommandContext): Promise<void> {
  if (!ctx.hasUI) {
    throw new Error("/launchpad login requires an interactive OMP session")
  }

  const child = Bun.spawn({
    cmd: [...bridgeCommand(), "login"],
    cwd: ctx.cwd,
    env: {
      ...process.env,
      CARGO_TERM_COLOR: "never",
    },
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
  })
  const reader = child.stdout.getReader()
  const decoder = new TextDecoder()
  const stderrPromise = new Response(child.stderr).text()
  let stdout = ""
  while (!stdout.includes(LOGIN_PROMPT)) {
    const chunk = await reader.read()
    if (chunk.done) break
    stdout += decoder.decode(chunk.value, { stream: true })
  }

  const authorizationUrl = stdout.match(LOGIN_URL)?.[0]
  if (!authorizationUrl) {
    const [stderr, exitCode] = await Promise.all([stderrPromise, child.exited])
    throw new Error(stderr.trim() || stdout.trim() || `Launchpad login exited with status ${exitCode}`)
  }

  const browserOpened = await openBrowser(pi, authorizationUrl, ctx.cwd)
  const authorized = await ctx.ui.confirm(
    "Launchpad login",
    `${browserOpened ? "Authorize OMP in the browser." : `Open ${authorizationUrl} in a browser.`}\n\nContinue after authorizing.`
  )
  if (!authorized) {
    child.kill()
    child.stdin.end()
    await child.exited
    ctx.ui.notify("Launchpad login cancelled", "warning")
    return
  }

  child.stdin.write("\n")
  child.stdin.end()
  while (true) {
    const chunk = await reader.read()
    if (chunk.done) break
    stdout += decoder.decode(chunk.value, { stream: true })
  }
  stdout += decoder.decode()
  const [stderr, exitCode] = await Promise.all([stderrPromise, child.exited])
  if (exitCode !== 0) {
    throw new Error(stderr.trim() || stdout.trim() || `Launchpad login exited with status ${exitCode}`)
  }
  ctx.ui.notify("Logged in to Launchpad", "info")
}

async function openBrowser(pi: ExtensionAPI, url: string, cwd: string): Promise<boolean> {
  let executable: string
  let commandArguments: string[]
  if (process.platform === "darwin") {
    executable = "open"
    commandArguments = [url]
  } else if (process.platform === "win32") {
    executable = "cmd"
    commandArguments = ["/c", "start", "", url]
  } else {
    executable = "xdg-open"
    commandArguments = [url]
  }
  const result = await pi.exec(executable, commandArguments, { cwd })
  return result.code === 0
}

async function runCommand(command: "logout" | "status", cwd: string): Promise<string> {
  const child = Bun.spawn({
    cmd: [...bridgeCommand(), command],
    cwd,
    env: {
      ...process.env,
      CARGO_TERM_COLOR: "never",
    },
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
  })
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ])
  if (exitCode !== 0) {
    throw new Error(stderr.trim() || stdout.trim() || `Launchpad command exited with status ${exitCode}`)
  }
  return stdout.trim()
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
