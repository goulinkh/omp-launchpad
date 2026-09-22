import { describe, expect, test } from "bun:test"
import type { ExtensionContext } from "@oh-my-pi/pi-coding-agent/extensibility/extensions"
import { LaunchpadStatusController } from "../src/status"

type StatusContext = Pick<ExtensionContext, "clearTimer" | "cwd" | "hasUI" | "setTimeout" | "ui">

function statusContext(updates: Array<string | undefined>, cwd = "/checkout"): StatusContext {
  return {
    cwd,
    hasUI: true,
    ui: {
      setStatus(_key, text) {
        updates.push(text)
      },
      theme: {},
    },
    setTimeout,
    clearTimer: clearTimeout,
  } as unknown as StatusContext
}

async function settle(): Promise<void> {
  await Promise.resolve()
  await Promise.resolve()
}

describe("LaunchpadStatusController", () => {
  test("shows the selected merge proposal with the Nerd Font rocket", async () => {
    const updates: Array<string | undefined> = []
    const controller = new LaunchpadStatusController(async () => ({
      details: { selected_proposal: { id: "500054" } },
    }))

    controller.refresh(statusContext(updates))
    await settle()

    expect(updates).toEqual(["\uf135 MP 500054"])
    controller.dispose()
  })

  test("drops same-checkout refreshes inside the TTL", async () => {
    let lookupCount = 0
    let now = 0
    const updates: Array<string | undefined> = []
    const controller = new LaunchpadStatusController(
      async () => {
        lookupCount++
        return { details: { selected_proposal: { id: "500054" } } }
      },
      () => now
    )
    const context = statusContext(updates)

    controller.refresh(context)
    await settle()
    controller.refresh(context)
    await settle()
    expect(lookupCount).toBe(1)

    now = 60_000
    controller.refresh(context)
    await settle()
    expect(lookupCount).toBe(2)
    controller.dispose()
  })

  test("queues a different checkout and discards the stale result", async () => {
    const first = Promise.withResolvers<{ details: { selected_proposal: { id: string } } }>()
    const second = Promise.withResolvers<{ details: { selected_proposal: { id: string } } }>()
    const lookups = [first.promise, second.promise]
    const updates: Array<string | undefined> = []
    const controller = new LaunchpadStatusController(async () => lookups.shift()!)
    const firstContext = statusContext(updates, "/first-checkout")
    const secondContext = statusContext(updates, "/second-checkout")

    controller.refresh(firstContext)
    controller.refresh(secondContext)
    first.resolve({ details: { selected_proposal: { id: "1" } } })
    await settle()
    expect(updates).toEqual([])

    second.resolve({ details: { selected_proposal: { id: "2" } } })
    await settle()
    expect(updates).toEqual(["\uf135 MP 2"])
    controller.dispose()
  })

  test("clears a previous status when lookup fails", async () => {
    const updates: Array<string | undefined> = []
    const controller = new LaunchpadStatusController(async () => {
      throw new Error("no Launchpad merge proposal")
    })

    controller.refresh(statusContext(updates))
    await settle()

    expect(updates).toEqual([undefined])
    controller.dispose()
  })

  test("allows an immediate refresh after clearing authentication state", async () => {
    let lookupCount = 0
    const updates: Array<string | undefined> = []
    const controller = new LaunchpadStatusController(
      async () => {
        lookupCount++
        return { details: { selected_proposal: { id: "500054" } } }
      },
      () => 0
    )
    const context = statusContext(updates)

    controller.refresh(context)
    await settle()
    controller.clear(context)
    controller.refresh(context)
    await settle()

    expect(lookupCount).toBe(2)
    expect(updates).toEqual(["\uf135 MP 500054", undefined, "\uf135 MP 500054"])
    controller.dispose()
  })

  test("aborts an active lookup during disposal", async () => {
    let lookupSignal: AbortSignal | undefined
    const controller = new LaunchpadStatusController(async signal => {
      lookupSignal = signal
      await new Promise<void>(() => {})
      return {}
    })

    controller.refresh(statusContext([]))
    expect(lookupSignal?.aborted).toBe(false)
    controller.dispose()
    expect(lookupSignal?.aborted).toBe(true)
  })
})
