// State core: engine event subscriptions with snapshots as the fallback
//
// Key patterns (safe under StrictMode's double mount):
// - The `__TAURI_INTERNALS__` guard has to come before the first listen
//   (without Tauri, listen throws synchronously)
// - The alive flag plus the add() helper: a subscription that resolves late
//   is unsubscribed at once, so nothing leaks or fires twice
// - Check alive before the initial snapshot's setState (unmount race)

import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api } from "../api";
import { EVENTS } from "../events";
import type { DeviceDto, PeerDto, SelfInfoDto, SyncedDto } from "../types";

/** Whether we are running inside the Tauri runtime (false during visual
 *  debugging in a plain browser, where it renders but wires up nothing) */
function hasTauri(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

/** lanecho's frontend state core */
export function useLanecho(opts?: { onSyncState?: (mode: string) => void }) {
  const [self, setSelf] = useState<SelfInfoDto | null>(null);
  const [devices, setDevices] = useState<DeviceDto[]>([]);
  // Pairing request queue: one dialog at a time, and a new key forces a
  // remount
  const [pairRequests, setPairRequests] = useState<PeerDto[]>([]);
  const [lastSync, setLastSync] = useState<SyncedDto | null>(null);

  // The callback is relayed through a ref so the effect's closure never goes
  // stale
  const onSyncStateRef = useRef(opts?.onSyncState);
  onSyncStateRef.current = opts?.onSyncState;

  useEffect(() => {
    if (!hasTauri()) return;
    let alive = true;
    const unsubs: UnlistenFn[] = [];
    const add = (subscription: Promise<UnlistenFn>) => {
      subscription
        .then((unsub) => {
          if (alive) {
            unsubs.push(unsub);
          } else {
            unsub();
          }
        })
        .catch(console.error);
    };
    const refetchDevices = () => {
      api
        .listDevices()
        .then((list) => {
          if (alive) setDevices(list);
        })
        .catch(console.error);
    };

    // Initial snapshot (the fallback outside the event stream)
    api
      .getSelfInfo()
      .then((info) => {
        if (alive) setSelf(info);
      })
      .catch(console.error);
    refetchDevices();
    // Catch up on pairing requests: a pair-requested event that arrived
    // before this component mounted (the startup window) had no listener and
    // is gone, so top the queue up from the engine's pending table — else the
    // peer waits for nothing until it times out
    api
      .listPendingPairs()
      .then((pending) => {
        if (!alive || pending.length === 0) return;
        setPairRequests((queue) => {
          const fresh = pending.filter(
            (p) => !queue.some((r) => r.fingerprint === p.fingerprint),
          );
          return fresh.length ? [...queue, ...fresh] : queue;
        });
      })
      .catch(console.error);

    // Devices coming and going, and pairing state changes, all refetch (there
    // are few devices, so pulling the whole list is simple and reliable)
    add(listen(EVENTS.PEER_UP, refetchDevices));
    add(listen(EVENTS.PEER_DOWN, refetchDevices));
    add(listen(EVENTS.PAIRED, refetchDevices));
    add(listen(EVENTS.UNPAIRED, refetchDevices));
    add(
      listen<PeerDto>(EVENTS.PAIR_REQUESTED, (event) => {
        if (!alive) return;
        setPairRequests((queue) =>
          queue.some((r) => r.fingerprint === event.payload.fingerprint)
            ? queue
            : [...queue, event.payload],
        );
      }),
    );
    add(
      listen<SyncedDto>(EVENTS.CLIPBOARD_SYNCED, (event) => {
        if (alive) setLastSync(event.payload);
      }),
    );
    add(
      // The payload is the syncMode string (a bool payload would collapse
      // send/receive back into both)
      listen<string>(EVENTS.SYNC_STATE, (event) => {
        onSyncStateRef.current?.(event.payload);
      }),
    );

    return () => {
      alive = false;
      unsubs.forEach((unsub) => unsub());
    };
  }, []);

  /** Answer a pairing request and drop it from the queue */
  const respondPair = useCallback((fingerprint: string, accept: boolean) => {
    api.respondPair(fingerprint, accept).catch(console.error);
    setPairRequests((queue) => queue.filter((r) => r.fingerprint !== fingerprint));
  }, []);

  /** Re-fetch the local device info (refreshes after a rename) */
  const refreshSelf = useCallback(() => {
    if (!hasTauri()) return;
    api.getSelfInfo().then(setSelf).catch(console.error);
  }, []);

  /** Re-fetch the device list */
  const refreshDevices = useCallback(() => {
    if (!hasTauri()) return;
    api.listDevices().then(setDevices).catch(console.error);
  }, []);

  return { self, devices, pairRequests, lastSync, respondPair, refreshSelf, refreshDevices };
}
