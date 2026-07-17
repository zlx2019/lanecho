// 设备列表: 在线状态 + 配对/解除交互(配对流程的状态与错误内聚于此,
// App 只提供数据源与刷新回调)

import { useState } from "react";
import { api } from "../api";
import { formatError, useI18n } from "../i18n";
import { Button } from "./ModalShell";
import type { DeviceDto } from "../types";

/** 设备列表分区(含标题与配对错误行) */
export function DeviceList({
  devices,
  onChanged,
}: {
  devices: DeviceDto[];
  /** 配对/解除成功后的列表刷新回调 */
  onChanged: () => void;
}) {
  const { t } = useI18n();
  const [pairingWith, setPairingWith] = useState<string | null>(null);
  const [pairError, setPairError] = useState("");

  /** 发起配对(等待对端确认, 按钮呈等待态) */
  const pair = async (device: DeviceDto) => {
    setPairError("");
    setPairingWith(device.fingerprint);
    try {
      await api.pairDevice(device.fingerprint);
      onChanged();
    } catch (e) {
      setPairError(formatError(e));
    } finally {
      setPairingWith(null);
    }
  };

  /** 解除配对 */
  const unpair = (device: DeviceDto) => {
    setPairError("");
    api
      .unpairDevice(device.fingerprint)
      .then(onChanged)
      .catch((e) => setPairError(formatError(e)));
  };

  return (
    <>
      <div className="gauge-label mt-5 mb-2">{t.devices.section}</div>
      <div className="rounded-xl border border-line bg-panel">
        {devices.length === 0 ? (
          <div className="px-4 py-6 text-center text-xs text-mist">{t.devices.empty}</div>
        ) : (
          devices.map((device) => (
            <div
              key={device.fingerprint}
              className="flex items-center gap-3 border-b border-line/40 px-4 py-2.5 last:border-b-0"
            >
              <span
                className={`size-2 shrink-0 rounded-full ${
                  device.online ? "anim-breathe bg-live" : "bg-faint"
                }`}
                title={device.online ? t.devices.online : t.devices.offline}
              />
              <div className="min-w-0 flex-1">
                <div className={`truncate text-sm ${device.online ? "text-fog" : "text-mist"}`}>
                  {device.name}
                </div>
                <div className="font-gauge text-[10px] text-mist">
                  {device.fingerprint.slice(0, 8)}
                  {device.platform ? ` · ${device.platform}` : ""}
                  {device.osVersion ? ` · ${device.osVersion}` : ""}
                </div>
              </div>
              {device.paired && (
                <span className="shrink-0 rounded bg-chip px-1.5 py-0.5 text-[10px] text-sonar">
                  {t.devices.paired}
                </span>
              )}
              {device.paired ? (
                <Button variant="danger" onClick={() => unpair(device)}>
                  {t.devices.unpair}
                </Button>
              ) : (
                device.online && (
                  <Button
                    variant="primary"
                    disabled={pairingWith !== null}
                    onClick={() => pair(device)}
                  >
                    {pairingWith === device.fingerprint ? t.devices.pairing : t.devices.pair}
                  </Button>
                )
              )}
            </div>
          ))
        )}
      </div>
      {pairError && <div className="mt-2 text-xs text-alert">{pairError}</div>}
    </>
  );
}
