<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { domainOf, relTime, type ChangeItem } from './api';

  export let changes: ChangeItem[];
  const dispatch = createEventDispatcher<{ open: ChangeItem; seen: number; seenAll: void }>();

  $: unseen = changes.filter((c) => !c.seen_at);
  $: seen = changes.filter((c) => c.seen_at);
</script>

{#if !changes.length}
  <div class="empty">
    <p>No changes yet.</p>
    <p class="hint">When a watched page changes, it shows up here.</p>
  </div>
{:else}
  {#if unseen.length}
    <div class="section">
      <span>Unreviewed</span>
      <button class="link" on:click={() => dispatch('seenAll')}>Mark all seen</button>
    </div>
    {#each unseen as c (c.id)}
      <button class="row unseen" on:click={() => dispatch('open', c)}>
        <span class="dot">●</span>
        <span class="body">
          <span class="top">
            <span class="title">{c.title ?? domainOf(c.url)}</span>
            <span class="domain">{domainOf(c.url)}</span>
          </span>
          {#if c.snippet}<span class="snippet">“{c.snippet}”</span>{/if}
        </span>
        <span class="meta">
          <span class="delta"><em class="add">+{c.lines_added}</em> <em class="del">−{c.lines_removed}</em></span>
          <span class="when">{relTime(c.detected_at)}</span>
          <span
            class="seenbtn"
            role="button"
            tabindex="-1"
            title="Mark seen"
            on:click|stopPropagation={() => dispatch('seen', c.id)}
            on:keydown|stopPropagation
          >✓</span>
        </span>
      </button>
    {/each}
  {/if}
  {#if seen.length}
    <div class="section"><span>Reviewed</span></div>
    {#each seen as c (c.id)}
      <button class="row" on:click={() => dispatch('open', c)}>
        <span class="dot seen">○</span>
        <span class="body">
          <span class="top">
            <span class="title">{c.title ?? domainOf(c.url)}</span>
            <span class="domain">{domainOf(c.url)}</span>
          </span>
        </span>
        <span class="meta">
          <span class="delta"><em class="add">+{c.lines_added}</em> <em class="del">−{c.lines_removed}</em></span>
          <span class="when">{relTime(c.detected_at)}</span>
        </span>
      </button>
    {/each}
  {/if}
{/if}

<style>
  .empty {
    text-align: center;
    padding: 80px 20px;
    color: var(--muted);
  }
  .hint {
    font-size: 12.5px;
  }
  .section {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 12px 16px 6px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
  }
  .link {
    border: none;
    background: none;
    color: var(--accent);
    cursor: pointer;
    font-size: 12px;
    text-transform: none;
    letter-spacing: 0;
    padding: 0;
  }
  .row {
    display: flex;
    width: 100%;
    align-items: center;
    gap: 10px;
    padding: 10px 16px;
    border: none;
    border-bottom: 1px solid var(--line);
    background: transparent;
    color: var(--fg);
    text-align: left;
    cursor: pointer;
    font: inherit;
  }
  .row:hover {
    background: var(--accent-soft);
  }
  .dot {
    color: var(--accent);
    font-size: 10px;
    width: 12px;
  }
  .dot.seen {
    color: var(--muted);
  }
  .body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .top {
    display: flex;
    gap: 8px;
    align-items: baseline;
    min-width: 0;
  }
  .title {
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .unseen .title {
    font-weight: 700;
  }
  .domain {
    color: var(--muted);
    font-size: 12px;
    flex-shrink: 0;
  }
  .snippet {
    color: var(--muted);
    font-size: 12.5px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .meta {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-shrink: 0;
  }
  .delta em {
    font-style: normal;
    font-size: 12px;
    font-variant-numeric: tabular-nums;
  }
  .add {
    color: var(--green);
  }
  .del {
    color: var(--red);
  }
  .when {
    color: var(--muted);
    font-size: 12px;
    min-width: 60px;
    text-align: right;
  }
  .seenbtn {
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 2px 8px;
    color: var(--muted);
  }
  .seenbtn:hover {
    color: var(--green);
    border-color: var(--green);
  }
</style>
