<script lang="ts">
  import { createEventDispatcher, onMount } from 'svelte';
  import { api, domainOf, relTime, type ChangeItem } from './api';

  export let change: ChangeItem;
  const dispatch = createEventDispatcher<{ close: void; seen: number }>();

  let diff = '';
  let error = '';

  onMount(async () => {
    try {
      diff = await api.getDiff(change.id);
    } catch (e) {
      error = String(e);
    }
  });

  function lineClass(line: string): string {
    if (line.startsWith('+++') || line.startsWith('---')) return 'file';
    if (line.startsWith('+')) return 'add';
    if (line.startsWith('-')) return 'del';
    if (line.startsWith('@')) return 'hunk';
    return '';
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') dispatch('close');
    if (e.key.toLowerCase() === 'e' && !change.seen_at) dispatch('seen', change.id);
  }
</script>

<svelte:window on:keydown={onKey} />

<div class="scrim" role="presentation" on:click={() => dispatch('close')}>
  <div class="panel" role="dialog" on:click|stopPropagation>
    <header>
      <div class="info">
        <h2>{change.title ?? domainOf(change.url)}</h2>
        <div class="sub">
          <span class="add">+{change.lines_added}</span>
          <span class="del">−{change.lines_removed}</span>
          · {relTime(change.detected_at)} · <span class="url">{change.url}</span>
        </div>
      </div>
      <div class="actions">
        {#if !change.seen_at}
          <button class="primary" on:click={() => dispatch('seen', change.id)}>Mark seen (e)</button>
        {/if}
        <button on:click={() => dispatch('close')}>Close (esc)</button>
      </div>
    </header>
    <div class="diff">
      {#if error}
        <p class="err">{error}</p>
      {:else if !diff}
        <p class="loading">Loading…</p>
      {:else}
        {#each diff.split('\n') as line}
          <div class="line {lineClass(line)}">{line || ' '}</div>
        {/each}
      {/if}
    </div>
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.35);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 40;
  }
  .panel {
    width: min(920px, 94vw);
    height: min(720px, 92vh);
    background: var(--panel);
    border-radius: 12px;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    box-shadow: 0 12px 48px rgba(0, 0, 0, 0.35);
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    padding: 14px 18px;
    border-bottom: 1px solid var(--line);
  }
  .info {
    min-width: 0;
  }
  h2 {
    margin: 0 0 3px;
    font-size: 15px;
  }
  .sub {
    color: var(--muted);
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .url {
    user-select: all;
  }
  .add {
    color: var(--green);
  }
  .del {
    color: var(--red);
  }
  .actions {
    display: flex;
    gap: 8px;
    flex-shrink: 0;
  }
  button {
    font: inherit;
    padding: 6px 12px;
    border-radius: 8px;
    border: 1px solid var(--line);
    background: var(--panel);
    color: var(--fg);
    cursor: pointer;
  }
  .primary {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .diff {
    flex: 1;
    overflow: auto;
    padding: 12px 0;
    font-family: ui-monospace, 'SF Mono', Menlo, monospace;
    font-size: 12px;
    line-height: 1.55;
  }
  .line {
    padding: 0 18px;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .line.add {
    background: var(--green-bg);
    color: var(--green);
  }
  .line.del {
    background: var(--red-bg);
    color: var(--red);
  }
  .line.hunk {
    color: var(--accent);
    margin-top: 8px;
  }
  .line.file {
    color: var(--muted);
  }
  .err {
    color: var(--red);
    padding: 0 18px;
  }
  .loading {
    color: var(--muted);
    padding: 0 18px;
  }
</style>
