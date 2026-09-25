import type {
  ExtensionAPI,
  ExtensionCommandContext,
} from "@oh-my-pi/pi-coding-agent/extensibility/extensions"
import { existsSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import { LaunchpadStatusController } from "./src/status"

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
  code?: "not_authenticated"
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
  const status = new LaunchpadStatusController((signal, cwd) =>
    runBridge({ op: "current_merge_proposal" }, signal, cwd)
  )
  configureInlineStatus(pi)

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
          if (await login(pi, ctx)) {
            status.clear(ctx)
            status.refresh(ctx)
          }
          return
        }
        if (command === "logout" || command === "status") {
          const output = await runCommand(command, ctx.cwd)
          ctx.ui.notify(output, "info")
          if (command === "logout") {
            status.clear(ctx)
          } else {
            status.refresh(ctx)
          }
          return
        }
        ctx.ui.notify("Usage: /launchpad login | logout | status", "warning")
      } catch (error) {
        ctx.ui.notify(error instanceof Error ? error.message : String(error), "error")
      }
    },
  })

  pi.on("session_start", (_event, ctx) => {
    status.refresh(ctx)
  })
  pi.on("session_switch", (_event, ctx) => {
    status.refresh(ctx)
  })
  pi.on("turn_end", (_event, ctx) => {
    status.refresh(ctx)
  })
  pi.on("tool_execution_end", (event, ctx) => {
    if (event.toolName === "launchpad_write" && !event.isError) {
      status.refresh(ctx)
    }
  })
  pi.on("session_shutdown", () => {
    status.dispose()
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
  const limitInteger = positiveInteger.max(1000)
  const diffSide = z.enum(["original", "modified"])
  const mergeProposalTarget = z.union([z.string(), positiveInteger])
  const proposalSelection = {
    status: oneOrManyStrings.optional(),
    target_branch: z.string().optional(),
    latest: z.boolean().optional(),
    include_superseded: z.boolean().optional(),
  }
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
      "Read Launchpad through lpcli. Repository inputs accept Launchpad paths and Git remote URLs. Branch and current " +
      "checkout lookup use explicit proposal selection rules and report the selected proposal and reason. Discussion " +
      "defaults to a compact review summary; format can request structured data or both. Merge proposal targets accept " +
      "a full Launchpad URL/path or a numeric ID when the working checkout has a related Launchpad remote. Prefer read " +
      "with lp:// URLs for individual bugs, merge proposals, and diff text.",
    parameters: z.union([
      z.object({
        op: z.literal("resource_view"),
        target: mergeProposalTarget,
        preview_diff_id: positiveInteger
          .optional()
          .describe("Selects that preview-diff snapshot's diff; the merge-proposal target needs no /diff suffix."),
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
        limit: limitInteger.optional(),
        ...withIntent,
      }),
      z.object({
        op: z.literal("search_merge_proposals"),
        repository: z.string(),
        status: oneOrManyStrings.optional(),
        limit: limitInteger.optional(),
        ...withIntent,
      }),
      z.object({
        op: z.literal("merge_proposal_for_branch"),
        repository: z.string().optional(),
        branch: z.string().optional(),
        ...proposalSelection,
        ...withIntent,
      }),
      z.object({
        op: z.literal("current_merge_proposal"),
        ...proposalSelection,
        ...withIntent,
      }),
      z.object({
        op: z.literal("merge_proposal_discussion"),
        target: mergeProposalTarget.optional(),
        repository: z.string().optional(),
        branch: z.string().optional(),
        format: z.enum(["summary", "structured", "both"]).optional(),
        current_diff_only: z.boolean().optional(),
        unresolved_only: z.boolean().optional(),
        comments: z.enum(["all", "general", "inline"]).optional(),
        since: z.string().optional(),
        reviewer: z.string().optional(),
        ...proposalSelection,
        ...withIntent,
      }),
      z.object({ op: z.literal("preview_diffs"), target: mergeProposalTarget, ...withIntent }),
      z.object({
        op: z.literal("inline_comments"),
        target: mergeProposalTarget,
        preview_diff_id: positiveInteger,
        ...withIntent,
      }),
      z.object({
        op: z.literal("review_drafts"),
        target: mergeProposalTarget,
        preview_diff_id: positiveInteger,
        ...withIntent,
      }),
      z.object({
        op: z.literal("diff_line_map"),
        target: mergeProposalTarget,
        preview_diff_id: positiveInteger,
        path: z.string(),
        file_line: positiveInteger,
        side: diffSide,
        ...withIntent,
      }),
    ]),
    approval: "read",
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      const request = normalizeBridgeRequest(params as BridgeRequest)
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
        target: mergeProposalTarget,
        body: z.string(),
        subject: z.string().optional(),
        vote: reviewVote.optional(),
        ...withIntent,
      }),
      z.object({
        op: z.literal("review_draft_update"),
        target: mergeProposalTarget,
        preview_diff_id: positiveInteger,
        path: z.string(),
        file_line: positiveInteger,
        side: diffSide,
        body: z.string().optional(),
        ...withIntent,
      }),
      z.object({
        op: z.literal("review_submit"),
        target: mergeProposalTarget,
        preview_diff_id: positiveInteger,
        body: z.string().optional(),
        vote: reviewVote.optional(),
        ...withIntent,
      }),
      z.object({
        op: z.literal("set_merge_proposal_status"),
        target: mergeProposalTarget,
        status: z.string(),
        ...withIntent,
      }),
      z.object({
        op: z.literal("merge_proposal_checkout"),
        target: mergeProposalTarget,
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
      const request = normalizeBridgeRequest(params as BridgeRequest)
      return bridgeResult(await runBridge(request, signal, ctx.cwd), true)
    },
  })
}
async function login(pi: ExtensionAPI, ctx: ExtensionCommandContext): Promise<boolean> {
  if (!ctx.hasUI) {
    throw new Error("/launchpad login requires an interactive OMP session")
  }

  const env = { ...process.env, CARGO_TERM_COLOR: "never" }
  const child = Bun.spawn({
    cmd: [...bridgeCommand(ctx.cwd, env), "login"],
    cwd: ctx.cwd,
    env,
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
    return false
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
  return true
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
  const env = { ...process.env, CARGO_TERM_COLOR: "never" }
  const child = Bun.spawn({
    cmd: [...bridgeCommand(cwd, env), command],
    cwd,
    env,
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

/** Keep the active layout while moving all extension statuses from an extra row into the status bar. */
function configureInlineStatus(pi: ExtensionAPI): void {
  const { settings, getPreset } = pi.pi
  if (!settings.get("statusLine.showHookStatus")) return

  const preset = settings.get("statusLine.preset")
  const definition = getPreset(preset)
  const left = preset === "custom" ? settings.get("statusLine.leftSegments") : definition.leftSegments
  const right = preset === "custom" ? settings.get("statusLine.rightSegments") : definition.rightSegments
  if (!left.includes("status") && !right.includes("status")) {
    const gitIndex = left.indexOf("git")
    const presetOptions: Record<string, unknown> = { ...definition.segmentOptions }
    const configuredOptions = Object.fromEntries(
      Object.entries(settings.get("statusLine.segmentOptions")).map(([name, value]) => {
        const defaults = presetOptions[name]
        return [
          name,
          typeof defaults === "object" && defaults !== null &&
          typeof value === "object" && value !== null && !Array.isArray(value)
            ? { ...defaults, ...value }
            : value,
        ]
      })
    )
    settings.override("statusLine.preset", "custom")
    const inlineLeft = [...left]
    inlineLeft.splice(gitIndex < 0 ? inlineLeft.length : gitIndex + 1, 0, "status")
    settings.override("statusLine.leftSegments", inlineLeft)
    settings.override("statusLine.rightSegments", [...right])
    settings.override("statusLine.segmentOptions", { ...presetOptions, ...configuredOptions })
  }
  settings.override("statusLine.showHookStatus", false)
}

function isLaunchpadUrl(path: unknown): path is string {
  return typeof path === "string" && path.startsWith("lp://")
}

function normalizeBridgeRequest(request: BridgeRequest): BridgeRequest {
  return typeof request.target === "number"
    ? { ...request, target: String(request.target) }
    : request
}

async function runBridge(
  request: BridgeRequest,
  signal: AbortSignal | undefined,
  cwd: string
): Promise<BridgeSuccess> {
  const env = { ...process.env, CARGO_TERM_COLOR: "never" }
  const command = bridgeCommand(cwd, env)
  const child = Bun.spawn({
    cmd: command,
    cwd,
    env,
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
    const failure = stderr.trim() || stdout.trim() || `Rust bridge exited with status ${exitCode}`
    const oldCompiler = command.length > 1 && (
      /^error: rustc \d+\.\d+(?:\.\d+)? is not supported by the following packages?:/m.test(stderr) ||
      /^error: package .+ cannot be built because it requires rustc \d+\.\d+(?:\.\d+)? or newer, while the currently active rustc version is \d+\.\d+(?:\.\d+)?/m.test(stderr)
    )
    throw new Error(oldCompiler
      ? `${failure}\nCargo selected a Rust compiler too old for this build. Update the toolchain (e.g. rustup update stable) or select a newer compiler for this session; check CARGO_BUILD_RUSTC and Cargo [build] rustc if configured.`
      : failure)
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
    const message = payload.error || "Launchpad operation failed"
    if (payload.code === "not_authenticated") {
      throw new Error(`${message}\nRun /launchpad login in an interactive OMP session, or run this command in an interactive terminal${process.platform === "win32" ? " (PowerShell)" : ""}:\n${formatTerminalCommand([...command, "login"], cwd)}`)
    }
    throw new Error(message)
  }
  return payload
}

function bridgeCommand(cwd: string, env: typeof process.env): string[] {
  if (env.OMP_LAUNCHPAD_BINARY) {
    return [env.OMP_LAUNCHPAD_BINARY]
  }
  const packagedBinary = join(
    PLUGIN_DIR,
    "bin",
    `omp-launchpad-${process.platform}-${process.arch}${process.platform === "win32" ? ".exe" : ""}`
  )
  if (existsSync(packagedBinary)) {
    return [packagedBinary]
  }
  const cargo = Bun.which("cargo", { ...(env.PATH === undefined ? {} : { PATH: env.PATH }), cwd })
  if (!cargo) {
    throw new Error("cargo is not installed. Install Rust 1.88 or newer and ensure cargo is on PATH.")
  }
  return [cargo, "run", "--quiet", "--release", "--manifest-path", MANIFEST_PATH, "--"]
}


function formatTerminalCommand(command: string[], cwd: string): string {
  if (process.platform === "win32") {
    const quote = (arg: string) => `'${arg.replaceAll("'", "''")}'`
    return `Set-Location -LiteralPath ${quote(cwd)}; if ($?) { & ${command.map(quote).join(" ")} }`
  }
  const quote = (arg: string) => `'${arg.replaceAll("'", `'"'"'`)}'`
  return `cd -- ${quote(cwd)} && ${command.map(quote).join(" ")}`
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
