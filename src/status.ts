import type { ExtensionContext } from "@oh-my-pi/pi-coding-agent/extensibility/extensions"

const ROCKET_ICON = "\uf135"
const LOOKUP_TIMEOUT_MS = 10_000
const REFRESH_TTL_MS = 60_000
const STATUS_KEY = "launchpad-merge-proposal"

interface StatusPayload {
  details?: Record<string, unknown>
}

type StatusLookup = (signal: AbortSignal, cwd: string) => Promise<StatusPayload>
type Clock = () => number
type StatusContext = Pick<ExtensionContext, "clearTimer" | "cwd" | "hasUI" | "setTimeout" | "ui">

interface PendingRefresh {
  context: StatusContext
  generation: number
}

interface RecentRefresh {
  cwd: string
  timestamp: number
}

export class LaunchpadStatusController {
  readonly #lookup: StatusLookup
  readonly #now: Clock
  #activeController: AbortController | undefined
  #disposed = false
  #generation = 0
  #pending: PendingRefresh | undefined
  #recentRefresh: RecentRefresh | undefined

  constructor(lookup: StatusLookup, now: Clock = Date.now) {
    this.#lookup = lookup
    this.#now = now
  }

  refresh(context: StatusContext): void {
    if (this.#disposed || !context.hasUI) return

    const timestamp = this.#now()
    if (
      this.#recentRefresh?.cwd === context.cwd &&
      timestamp - this.#recentRefresh.timestamp < REFRESH_TTL_MS
    ) {
      return
    }
    this.#recentRefresh = { cwd: context.cwd, timestamp }

    const generation = ++this.#generation
    if (this.#activeController) {
      this.#pending = { context, generation }
      return
    }
    this.#start(context, generation)
  }

  clear(context: StatusContext): void {
    this.#generation++
    this.#pending = undefined
    this.#recentRefresh = undefined
    this.#activeController?.abort()
    context.ui.setStatus(STATUS_KEY, undefined)
  }

  dispose(): void {
    this.#disposed = true
    this.#generation++
    this.#pending = undefined
    this.#activeController?.abort()
  }

  #start(context: StatusContext, generation: number): void {
    const controller = new AbortController()
    this.#activeController = controller
    const timer = context.setTimeout(() => controller.abort(), LOOKUP_TIMEOUT_MS)

    void this.#resolve(context, generation, controller, timer)
  }

  async #resolve(
    context: StatusContext,
    generation: number,
    controller: AbortController,
    timer: Timer
  ): Promise<void> {
    let text: string | undefined
    try {
      const payload = await this.#lookup(controller.signal, context.cwd)
      text = proposalStatusText(payload)
    } catch {
      text = undefined
    } finally {
      context.clearTimer(timer)
      if (this.#activeController === controller) {
        this.#activeController = undefined
      }
    }

    if (!this.#disposed && generation === this.#generation) {
      context.ui.setStatus(STATUS_KEY, text)
    }

    const pending = this.#pending
    this.#pending = undefined
    if (!this.#disposed && pending) {
      this.#start(pending.context, pending.generation)
    }
  }
}

function proposalStatusText(payload: StatusPayload): string | undefined {
  const proposal = payload.details?.selected_proposal
  if (typeof proposal !== "object" || proposal === null || !("id" in proposal)) return undefined

  const id = proposal.id
  const proposalId =
    typeof id === "string" && /^\d+$/.test(id)
      ? id
      : typeof id === "number" && Number.isSafeInteger(id) && id > 0
        ? String(id)
        : undefined
  if (!proposalId) return undefined

  return `${ROCKET_ICON} MP ${proposalId}`
}
