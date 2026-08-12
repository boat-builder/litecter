<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { api, domainOf, relTime, type UrlRow } from './api';

  export let urls: UrlRow[];
  export let unseenIds: Set<number>;
  const dispatch = createEventDispatcher<{ refresh: void; flash: string }>();

  let query = '';
  let selected = new Set<number>();
  let bulkSchedule = '';
  let searchInput: HTMLInputElement;

  export function focusSearch() {
    searchInput?.focus();
  }

  $: filtered = query.trim()
    ? urls.filter((u) => {
        const q = query.toLowerCase();
        return u.url.toLowerCase().includes(q) || (u.title ?? '').toLowerCase().includes(q);
      })
    : urls;

  $: allVisibleSelected = filtered.length > 0 && filtered.every((u) => selected.has(u.id));

  function toggle(id: number) {
    if (selected.has(id)) selected.delete(id);
    else selected.add(id);
    selected = selected;
  }

  function toggleAll() {
    if (allVisibleSelected) selected = new Set();
    else selected = new Set(filtered.map((u) => u.id));
  }

  async function changeSchedule(ids: number[], every: string) {
    if (!every) return;
    await api.setSchedule(ids, every);
    bulkSchedule = '';
    dispatch('refresh');
    dispatch('flash', `Schedule → ${every} for ${ids.length} URL(s)`);
  }

  async function removeSelected() {
    const n = selected.size;
    if (!n || !confirm(`Stop watching ${n} URL(s)? Their history is deleted.`)) return;
    await api.removeUrls([...selected]);
    selected = new Set();
    dispatch('refresh');
    dispatch('flash', `Removed ${n} URL(s)`);
  }

  async function checkSelected(ids: number[]) {
    await api.checkNow(ids);
    dispatch('flash', `Checking ${ids.length} URL(s) in the background…`);
  }
</script>

<div class="toolbar">
  <input
    bind:this={searchInput}
    bind:value={query}
    placeholder="Filter by title or url…  ( / )"
    spellcheck="false"
  />
  {#if selected.size}
    <div class="bulk">
      <span>{selected.size} selected</span>
      <select bind:value={bulkSchedule} on:change={() => changeSchedule([...selected], bulkSchedule)}>
        <option value="" disabled selected>schedule…</option>
        <option value="hourly">hourly</option>
        <option value="daily">daily</option>
        <option value="weekly">weekly</option>
        <option value="monthly">monthly</option>
      </select>
      <button on:click={() => checkSelected([...selected])}>Check now</button>
      <button class="danger" on:click={removeSelected}>Remove</button>
    </div>
  {:else}
    <button on:click={() => api.checkNow(null).then(() => dispatch('flash', 'Checking due URLs…'))}>
      Check due now
    </button>
  {/if}
</div>

{#if !urls.length}
  <div class="empty">
    <p>Nothing watched yet.</p>
    <p class="hint">Paste a URL in the bar above — or a whole list of them.</p>
  </div>
{:else}
  <table>
    <thead>
      <tr>
        <th class="check"><input type="checkbox" checked={allVisibleSelected} on:change={toggleAll} /></th>
        <th>Page</th>
        <th class="sched">Every</th>
        <th class="time">Last check</th>
        <th class="time">Next check</th>
      </tr>
    </thead>
    <tbody>
      {#each filtered as u (u.id)}
        <tr class:err={u.status === 'error'}>
          <td class="check">
            <input type="checkbox" checked={selected.has(u.id)} on:change={() => toggle(u.id)} />
          </td>
          <td class="page">
            <span class="titleline">
              {#if unseenIds.has(u.id)}<span class="dot" title="Unreviewed change">●</span>{/if}
              <span class="title">{u.title ?? domainOf(u.url)}</span>
              <span class="domain">{u.url}</span>
            </span>
            {#if u.status === 'error' && u.error_message}
              <span class="errmsg" title={u.error_message}>⚠ {u.error_message}</span>
            {/if}
          </td>
          <td class="sched">
            <select value={u.schedule} on:change={(e) => changeSchedule([u.id], e.currentTarget.value)}>
              <option value="hourly">hourly</option>
              <option value="daily">daily</option>
              <option value="weekly">weekly</option>
              <option value="monthly">monthly</option>
            </select>
          </td>
          <td class="time">{u.last_checked_at ? relTime(u.last_checked_at) : 'never'}</td>
          <td class="time">{relTime(u.next_check_at)}</td>
        </tr>
      {/each}
    </tbody>
  </table>
  {#if query && !filtered.length}
    <div class="empty"><p>No matches for “{query}”.</p></div>
  {/if}
{/if}

<style>
  .toolbar {
    display: flex;
    gap: 10px;
    align-items: center;
    padding: 10px 16px;
    border-bottom: 1px solid var(--line);
    position: sticky;
    top: 0;
    background: var(--panel);
    z-index: 5;
  }
  .toolbar input {
    flex: 1;
    max-width: 340px;
    padding: 6px 10px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    outline: none;
  }
  .toolbar input:focus {
    border-color: var(--accent);
  }
  .bulk {
    display: flex;
    gap: 8px;
    align-items: center;
    color: var(--muted);
    font-size: 12.5px;
  }
  button,
  select {
    font: inherit;
    font-size: 12.5px;
    padding: 5px 10px;
    border-radius: 7px;
    border: 1px solid var(--line);
    background: var(--panel);
    color: var(--fg);
    cursor: pointer;
  }
  .danger {
    color: var(--red);
    border-color: var(--red);
  }
  .empty {
    text-align: center;
    padding: 80px 20px;
    color: var(--muted);
  }
  .hint {
    font-size: 12.5px;
  }
  table {
    width: 100%;
    border-collapse: collapse;
  }
  th {
    text-align: left;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
    padding: 8px 10px;
    border-bottom: 1px solid var(--line);
    font-weight: 600;
  }
  td {
    padding: 7px 10px;
    border-bottom: 1px solid var(--line);
    vertical-align: middle;
  }
  .check {
    width: 34px;
    padding-left: 16px;
  }
  .page {
    min-width: 0;
  }
  .titleline {
    display: flex;
    gap: 8px;
    align-items: baseline;
    min-width: 0;
  }
  .dot {
    color: var(--accent);
    font-size: 9px;
  }
  .title {
    font-weight: 600;
    white-space: nowrap;
  }
  .domain {
    color: var(--muted);
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .errmsg {
    display: block;
    color: var(--warn);
    font-size: 12px;
    margin-top: 2px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 480px;
  }
  .sched {
    width: 110px;
  }
  .time {
    width: 100px;
    color: var(--muted);
    font-size: 12.5px;
    white-space: nowrap;
  }
  tr.err .title {
    color: var(--warn);
  }
</style>
