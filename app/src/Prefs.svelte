<script lang="ts">
  import { createEventDispatcher, onMount } from 'svelte';
  import { api } from './api';

  const dispatch = createEventDispatcher<{ close: void; flash: string }>();

  let digestHour = 9;
  let autostart = true;
  let loaded = false;

  onMount(async () => {
    try {
      const p = await api.getPrefs();
      digestHour = p.digest_hour;
      autostart = p.autostart;
    } finally {
      loaded = true;
    }
  });

  async function save() {
    await api.setPrefs(digestHour, autostart);
    dispatch('flash', 'Settings saved');
    dispatch('close');
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') dispatch('close');
  }
</script>

<svelte:window on:keydown={onKey} />

<div class="scrim" role="presentation" on:click={() => dispatch('close')}>
  <div class="panel" role="dialog" on:click|stopPropagation>
    <h2>Settings</h2>
    {#if loaded}
      <label class="row">
        <span>Daily digest hour<br /><small>One notification per day while changes await review.</small></span>
        <select bind:value={digestHour}>
          {#each Array(24) as _, h}
            <option value={h}>{String(h).padStart(2, '0')}:00</option>
          {/each}
        </select>
      </label>
      <label class="row">
        <span>Launch at login<br /><small>Litecter starts hidden in the menu bar.</small></span>
        <input type="checkbox" bind:checked={autostart} />
      </label>
      <div class="actions">
        <button on:click={() => dispatch('close')}>Cancel</button>
        <button class="primary" on:click={save}>Save</button>
      </div>
    {:else}
      <p class="loading">Loading…</p>
    {/if}
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
    width: min(440px, 92vw);
    background: var(--panel);
    border-radius: 12px;
    padding: 20px;
    box-shadow: 0 12px 48px rgba(0, 0, 0, 0.35);
  }
  h2 {
    margin: 0 0 16px;
    font-size: 16px;
  }
  .row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 16px;
    padding: 10px 0;
    border-bottom: 1px solid var(--line);
  }
  small {
    color: var(--muted);
  }
  select,
  button {
    font: inherit;
    padding: 6px 10px;
    border-radius: 8px;
    border: 1px solid var(--line);
    background: var(--panel);
    color: var(--fg);
    cursor: pointer;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }
  .primary {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .loading {
    color: var(--muted);
  }
</style>
