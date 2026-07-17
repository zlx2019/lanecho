// 状态核心: 引擎事件订阅 + 快照兜底(deskmate useDeskmate 的订阅骨架继承)
//
// 关键模式(StrictMode 双挂载安全):
// - `__TAURI_INTERNALS__` 守卫必须先于第一个 listen(无 Tauri 时 listen 同步抛)
// - alive 标志 + add() 辅助: 迟到 resolve 的订阅立即退订, 防泄漏/重复
// - 初始快照 setState 前判 alive(卸载竞态)

import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api } from "../api";
import { EVENTS } from "../events";
import type { DeviceDto, PeerDto, SelfInfoDto, SyncedDto } from "../types";

/** 是否在 Tauri 运行时内(纯浏览器视觉调试时为 false, 只渲染不联动) */
function hasTauri(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

/** lanecho 前端状态核心 */
export function useLanecho(opts?: { onSyncState?: (enabled: boolean) => void }) {
  const [self, setSelf] = useState<SelfInfoDto | null>(null);
  const [devices, setDevices] = useState<DeviceDto[]>([]);
  // 配对请求队列: 逐个弹窗, key 换新强制重挂载
  const [pairRequests, setPairRequests] = useState<PeerDto[]>([]);
  const [lastSync, setLastSync] = useState<SyncedDto | null>(null);

  // 回调经 ref 转接, 避免 effect 闭包过期
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

    // 初始快照(事件流之外的兜底)
    api
      .getSelfInfo()
      .then((info) => {
        if (alive) setSelf(info);
      })
      .catch(console.error);
    refetchDevices();
    // 配对请求补拉: 本组件挂载前(启动窗口期)到达的 pair-requested
    // 事件无人监听已丢, 从引擎待决表补进队列, 对端才不会空等超时
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

    // 设备增删与配对状态变化: 统一 refetch(设备数少, 全量拉取简单可靠)
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
      listen<boolean>(EVENTS.SYNC_STATE, (event) => {
        onSyncStateRef.current?.(event.payload);
      }),
    );

    return () => {
      alive = false;
      unsubs.forEach((unsub) => unsub());
    };
  }, []);

  /** 回应配对请求并出队 */
  const respondPair = useCallback((fingerprint: string, accept: boolean) => {
    api.respondPair(fingerprint, accept).catch(console.error);
    setPairRequests((queue) => queue.filter((r) => r.fingerprint !== fingerprint));
  }, []);

  /** 重新拉取本机信息(改名后刷新) */
  const refreshSelf = useCallback(() => {
    if (!hasTauri()) return;
    api.getSelfInfo().then(setSelf).catch(console.error);
  }, []);

  /** 重新拉取设备列表 */
  const refreshDevices = useCallback(() => {
    if (!hasTauri()) return;
    api.listDevices().then(setDevices).catch(console.error);
  }, []);

  return { self, devices, pairRequests, lastSync, respondPair, refreshSelf, refreshDevices };
}
