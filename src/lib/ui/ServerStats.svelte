<script lang="ts">
  import type { StatsData } from "$lib/session/stats";
  import { lang, t } from "$lib/i18n";
  import {
    formatRateUnit,
    formatRateValue,
    formatDate,
    formatClock,
  } from "$lib/format";

  export let stats: StatsData | null = null;

  $: cpu = stats?.cpu ?? 0;
  $: memory = stats?.memory ?? 0;
  $: up = stats?.up ?? 0;
  $: down = stats?.down ?? 0;
  $: time = stats?.time ?? 0;

  /** Format the server time, splitting date and clock into two lines. */
  $: dateText = time > 0 ? formatDate(time) : "----:--:--";
  $: clockText = time > 0 ? formatClock(time) : "--:--:--";
</script>

<div class="flex items-center gap-[4.8px] select-none">
  <!-- CPU / memory usage (two lines, fixed width) -->
  <div
    class="flex w-[4.75rem] flex-col items-start justify-center gap-[3px] leading-tight whitespace-nowrap"
    title={t($lang, "stats.resources")}
  >
    <div class="font-mono text-[10px] text-zinc-300">
      <span class="label">{t($lang, "stats.cpu")}:</span>
      <span class="value">{cpu.toFixed(0)}</span>
      <span class="unit">%</span>
    </div>
    <div class="font-mono text-[10px] text-zinc-300">
      <span class="label">{t($lang, "stats.mem")}:</span>
      <span class="value">{memory.toFixed(0)}</span>
      <span class="unit">%</span>
    </div>
  </div>

  <!-- Network rate (two lines, fixed width) -->
  <div
    class="flex w-28 flex-col items-start justify-center gap-[3px] leading-tight whitespace-nowrap"
    title={t($lang, "stats.net")}
  >
    <div class="font-mono text-[10px] text-zinc-300">
      <span class="label">{t($lang, "stats.up")}:</span>
      <span class="value-wide">{formatRateValue(up)}</span>
      <span class="unit">{formatRateUnit(up)}</span>
    </div>
    <div class="font-mono text-[10px] text-zinc-300">
      <span class="label">{t($lang, "stats.down")}:</span>
      <span class="value-wide">{formatRateValue(down)}</span>
      <span class="unit">{formatRateUnit(down)}</span>
    </div>
  </div>

  <!-- Server time (two lines, fixed width) -->
  <div
    class="flex w-[64px] flex-col items-start justify-center gap-[3px] leading-tight whitespace-nowrap font-mono text-[10px] text-zinc-300"
    title={t($lang, "stats.time")}
  >
    <div>{dateText}</div>
    <div>{clockText}</div>
  </div>
</div>

<style lang="postcss">
  .label {
    @apply inline-block w-8 text-zinc-300;
  }

  .value {
    @apply inline-block text-right text-zinc-300;
    width: 1rem;
  }

  .value-wide {
    @apply inline-block text-right text-zinc-300;
    width: 1.75rem;
  }

  .unit {
    @apply inline-block text-left text-zinc-300;
    width: 1.25rem;
  }
</style>
