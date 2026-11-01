import { Show, type JSX } from "solid-js";
import type { VoiceLive, VoiceNetwork, VoicePower, VoiceSpecs, VoiceStatus } from "../../api/voice";
import { formatBytes, formatUptime, rssiQuality, type SignalQuality } from "../../lib/voice";
import { relativeTime } from "../../lib/relativeTime";
import { locale, t, type MessageKey } from "../../i18n";
import { Card } from "../page";

// The overview cards of the Voice device page: specs, live, network and power. Each row is a
// dt/dd pair on the page system's `pg-stat` rows; nothing here fetches.

function Row(props: { label: string; children: JSX.Element; class?: string }) {
  return (
    <div class={`pg-stat ${props.class ?? ""}`}>
      <dt>{props.label}</dt>
      <dd>{props.children}</dd>
    </div>
  );
}

const has = (v: unknown): boolean => v !== undefined && v !== null;
const yesNo = (v: boolean | undefined) => (v ? t("voice.yes") : t("voice.no"));

export function StatusDot(props: { online: boolean }) {
  return <span class="vd-dot" data-state={props.online ? "ok" : "idle"} aria-hidden="true" />;
}

export function OnlineWord(props: { online: boolean }) {
  return (
    <span class="vd-state" data-state={props.online ? "ok" : "idle"}>
      <StatusDot online={props.online} />
      {props.online ? t("voice.online") : t("voice.offline")}
    </span>
  );
}

export function SpecsCard(props: { specs: VoiceSpecs }) {
  const s = () => props.specs;
  return (
    <Card title={t("voice.specs")} class="vd-card vd-specs">
      <dl class="pg-stats">
        <Show when={s().chip}>
          <Row label={t("voice.chip")}>{t("voice.chipValue", { chip: s().chip!.toUpperCase(), rev: s().rev ?? "?" })}</Row>
        </Show>
        <Show when={has(s().cores)}>
          <Row label={t("voice.cores")}>{t("voice.coresValue", { cores: s().cores!, mhz: s().cpu_mhz ?? "?" })}</Row>
        </Show>
        <Show when={has(s().flash_bytes)}>
          <Row label={t("voice.flash")}>{formatBytes(s().flash_bytes!)}</Row>
        </Show>
        <Show when={has(s().psram_bytes)}>
          <Row label={t("voice.psram")}>{s().psram_bytes ? formatBytes(s().psram_bytes!) : t("voice.none")}</Row>
        </Show>
        <Show when={s().fw}>
          <Row label={t("voice.firmwareLabel")}>{t("voice.firmwareValue", { fw: s().fw!, commit: s().commit ?? "unknown" })}</Row>
        </Show>
        <Show when={s().mac}>
          <Row label={t("voice.mac")}>{s().mac}</Row>
        </Show>
        <Row label={t("voice.powerSensor")}>{s().ina ? s().ina!.toUpperCase() : t("voice.none")}</Row>
        <Row label={t("voice.tempSensor")}>{yesNo(s().temp_sensor)}</Row>
      </dl>
    </Card>
  );
}

const QUALITY_TEXT: Record<SignalQuality, MessageKey> = {
  excellent: "voice.signalExcellent",
  good: "voice.signalGood",
  fair: "voice.signalFair",
  weak: "voice.signalWeak",
};

/** The Wi-Fi signal as "-61 dBm, good" with the quality as a word, never colour alone. */
export function Signal(props: { rssi: number }) {
  const q = () => rssiQuality(props.rssi);
  return (
    <span class="vd-signal" data-level={q()}>
      {t("voice.wifiValue", { rssi: props.rssi, quality: t(QUALITY_TEXT[q()]) })}
    </span>
  );
}

export function LiveCard(props: { status: VoiceStatus }) {
  const s = () => props.status;
  const live = (): VoiceLive => s().live;
  const reported = () => Object.keys(live()).length > 0;
  return (
    <Card title={t("voice.live")} class="vd-card vd-live">
      <dl class="pg-stats">
        <Row label={t("voice.status")}>
          <OnlineWord online={s().online} />
          {" "}
          <span class="vd-since">
            {s().online && s().connected_since
              ? t("voice.onlineSince", { time: relativeTime(s().connected_since!) })
              : s().last_seen
                ? t("voice.lastSeen", { time: relativeTime(s().last_seen!) })
                : t("voice.neverSeen")}
          </span>
        </Row>
        <Show when={reported()}>
          <Show when={has(live().uptime_ms)}>
            <Row label={t("voice.uptime")}>{formatUptime(live().uptime_ms!)}</Row>
          </Show>
          <Show when={typeof live().rssi === "number"}>
            <Row label={t("voice.wifi")}><Signal rssi={live().rssi!} /></Row>
          </Show>
          <Show when={has(live().heap_free)}>
            <Row label={t("voice.heapFree")}>{formatBytes(live().heap_free!)}</Row>
          </Show>
          <Show when={has(live().heap_min_free)}>
            <Row label={t("voice.heapMin")}>{formatBytes(live().heap_min_free!)}</Row>
          </Show>
          <Show when={has(live().cpu_mhz)}>
            <Row label={t("voice.cpu")}>{t("voice.cpuValue", { mhz: live().cpu_mhz! })}</Row>
          </Show>
          <Show when={typeof live().temp_c === "number"}>
            <Row label={t("voice.temperature")}>{t("voice.temperatureValue", { temp: Math.round(live().temp_c! * 10) / 10 })}</Row>
          </Show>
        </Show>
      </dl>
      <Show when={!reported()}>
        <p class="pg-muted vd-note">{t("voice.noStats")}</p>
      </Show>
    </Card>
  );
}

export function NetworkCard(props: { network: VoiceNetwork }) {
  const n = () => props.network;
  return (
    <Card title={t("voice.network")} class="vd-card vd-network">
      <dl class="pg-stats">
        <Row label={t("voice.sentToday")}>{formatBytes(n().today_up_bytes)}</Row>
        <Row label={t("voice.receivedToday")}>{formatBytes(n().today_down_bytes)}</Row>
        <Row label={t("voice.sentTotal")}>{formatBytes(n().total_up_bytes)}</Row>
        <Row label={t("voice.receivedTotal")}>{formatBytes(n().total_down_bytes)}</Row>
      </dl>
    </Card>
  );
}

const nf = (n: number) => new Intl.NumberFormat(locale(), { maximumFractionDigits: n >= 100 ? 0 : 1 }).format(n);

export function PowerCard(props: { power: VoicePower; ina?: VoiceSpecs["ina"] }) {
  const p = () => props.power;
  return (
    <Card title={t("voice.power")} class="vd-card vd-power">
      <dl class="pg-stats">
        <Row label={t("voice.powerNow")}>
          <Show when={typeof p().est_mw === "number"} fallback="–">
            {t("voice.powerValue", { mw: nf(p().est_mw!) })}
          </Show>
          {" "}
          <span class="vd-badge" data-kind={p().measured ? "measured" : "estimated"}>
            {p().measured ? t("voice.measured") : t("voice.estimated")}
          </span>
        </Row>
        <Row label={t("voice.energyToday")}>
          <Show when={typeof p().energy_mwh_today === "number"} fallback="–">
            {t("voice.energyValue", { mwh: nf(p().energy_mwh_today!) })}
          </Show>
        </Row>
      </dl>
      <p class="vd-note pg-muted">
        {p().measured ? t("voice.powerMeasuredNote", { sensor: (props.ina ?? "INA219").toUpperCase() }) : t("voice.powerHowTo")}
      </p>
    </Card>
  );
}
