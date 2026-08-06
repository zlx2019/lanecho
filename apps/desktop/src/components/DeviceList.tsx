// Device list: online state plus the pair / unpair interaction (the pairing
// flow's own state and errors live here; App only supplies the data source
// and a refresh callback)
//
// It owns the settings window's devices tab outright, so it carries no
// section heading of its own — the tab navigation is the heading; the list
// also holds paired but offline devices, greyed out, not just online ones

import { useState } from "react";
import { api } from "../api";
import { formatError, useI18n } from "../i18n";
import { Button } from "./ModalShell";
import type { DeviceDto } from "../types";

/** Device list section (heading and pairing error row included) */
export function DeviceList({
  devices,
  onChanged,
}: {
  devices: DeviceDto[];
  /** Refresh callback, invoked after a successful pair / unpair */
  onChanged: () => void;
}) {
  const { t } = useI18n();
  const [pairingWith, setPairingWith] = useState<string | null>(null);
  const [pairError, setPairError] = useState("");

  /** Start pairing (waits for the peer to confirm, with the button in a
   *  waiting state) */
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

  /** Unpair */
  const unpair = (device: DeviceDto) => {
    setPairError("");
    api
      .unpairDevice(device.fingerprint)
      .then(onChanged)
      .catch((e) => setPairError(formatError(e)));
  };

  return (
    <>
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
