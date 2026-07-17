// 配对请求弹窗: 强制决策(不传 onClose, 点遮罩不可关 —— deskmate OfferModal 约定)

import { useI18n } from "../i18n";
import type { PeerDto } from "../types";
import { Button, ModalShell } from "./ModalShell";

/** 入站配对请求的确认弹窗 */
export function PairRequestModal({
  peer,
  onRespond,
}: {
  peer: PeerDto;
  onRespond: (fingerprint: string, accept: boolean) => void;
}) {
  const { t } = useI18n();
  return (
    <ModalShell title={t.pairing.title}>
      <div className="px-5 py-4">
        <div className="text-sm text-fog">{t.pairing.message(peer.name)}</div>
        <div className="mt-3 rounded-md border border-line/70 bg-abyss/50 px-3 py-2">
          <div className="gauge-label">{t.pairing.fingerprint}</div>
          <div className="mt-1 font-gauge text-xs break-all text-fog/90">
            {peer.fingerprint}
          </div>
        </div>
        <div className="mt-3 text-[11px] leading-relaxed text-mist">{t.pairing.hint}</div>
        <div className="mt-4 flex items-center justify-end gap-2">
          <Button variant="danger" onClick={() => onRespond(peer.fingerprint, false)}>
            {t.pairing.decline}
          </Button>
          <Button variant="primary" onClick={() => onRespond(peer.fingerprint, true)}>
            {t.pairing.accept}
          </Button>
        </div>
      </div>
    </ModalShell>
  );
}
