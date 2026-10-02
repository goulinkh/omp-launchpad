import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"

import { afterEach, beforeEach, describe, expect, spyOn, test } from "bun:test"
import { resetSettingsForTest, Settings } from "@oh-my-pi/pi-coding-agent/config/settings"
import { loadExtensions, type ExtensionContext } from "@oh-my-pi/pi-coding-agent/extensibility/extensions"
import { getPreset } from "@oh-my-pi/pi-coding-agent/modes/components"
import {
  cfgStatusLineLeftSegments,
  cfgStatusLinePreset,
  cfgStatusLineRightSegments,
  cfgStatusLineSegmentOptions,
  cfgStatusLineShowHookStatus,
} from "@oh-my-pi/pi-coding-agent/modes/settings"
import { LaunchpadStatusController } from "../src/status"

type StatusContext = Pick<ExtensionContext, "clearTimer" | "cwd" | "hasUI" | "setTimeout" | "ui">

function statusContext(updates: Array<string | undefined>, cwd = "/checkout"): StatusContext {
  return {
    cwd,
    hasUI: true,
    ui: {
      setStatus(_key: string, text: string | undefined) {
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

describe("extension session startup", () => {
  let cwd: string

  beforeEach(() => {
    resetSettingsForTest()
    cwd = mkdtempSync(join(tmpdir(), "launchpad-status-"))
  })

  afterEach(() => {
    resetSettingsForTest()
    rmSync(cwd, { recursive: true, force: true })
  })

  async function start(overrides: Record<string, unknown>): Promise<Settings> {
    const path = fileURLToPath(new URL("../index.ts", import.meta.url))
    const { extensions, errors } = await loadExtensions([path], cwd)
    if (errors.length) throw new Error(JSON.stringify(errors))
    const extension = extensions[0]
    if (!extension) throw new Error("Launchpad extension was not loaded")
    const handlers = extension.handlers.get("session_start")
    if (!handlers?.length) throw new Error("Launchpad session handler was not registered")
    const settings = await Settings.init({ inMemory: true, cwd, agentDir: cwd, overrides })
    const refresh = spyOn(LaunchpadStatusController.prototype, "refresh").mockImplementation(() => {})
    try {
      for (const handler of handlers) {
        await handler({ type: "session_start" }, statusContext([], cwd))
      }
    } finally {
      refresh.mockRestore()
    }
    return settings
  }

  test("moves hook statuses inline without losing preset options or user overrides", async () => {
    const settings = await start({
      "statusLine.preset": "minimal",
      "statusLine.segmentOptions": { git: { showStaged: true } },
    })
    const preset = getPreset("minimal")

    expect(cfgStatusLinePreset.get(settings)).toBe("custom")
    expect(cfgStatusLineLeftSegments.get(settings)).toEqual([...preset.leftSegments, "status"])
    expect(cfgStatusLineRightSegments.get(settings)).toEqual(preset.rightSegments)
    expect(cfgStatusLineSegmentOptions.get(settings)).toEqual({
      ...preset.segmentOptions,
      git: { ...preset.segmentOptions?.git, showStaged: true },
    })
    expect(cfgStatusLineShowHookStatus.get(settings)).toBe(false)
    expect(settings.getProvenance(cfgStatusLineLeftSegments)).toBe("runtime")
  })

  test("preserves an existing inline status segment in a custom layout", async () => {
    const settings = await start({
      "statusLine.preset": "custom",
      "statusLine.leftSegments": ["model", "git"],
      "statusLine.rightSegments": ["status", "cost"],
    })

    expect(cfgStatusLineLeftSegments.get(settings)).toEqual(["model", "git"])
    expect(cfgStatusLineRightSegments.get(settings)).toEqual(["status", "cost"])
    expect(cfgStatusLineShowHookStatus.get(settings)).toBe(false)
  })

  test("leaves the layout unchanged when hook status is explicitly disabled", async () => {
    const settings = await start({
      "statusLine.preset": "compact",
      "statusLine.showHookStatus": false,
    })

    expect(cfgStatusLinePreset.get(settings)).toBe("compact")
    expect(cfgStatusLineShowHookStatus.get(settings)).toBe(false)
  })
})
