<script>
  import Modal from "./Modal.svelte";

  let { prefs, theme = "system", onTheme = () => {}, onSave, onClose } = $props();

  // Edit a local copy; commit on Save.
  let p = $state({ ...prefs });

  function save() {
    onSave({ ...p, max_parallel: Math.min(8, Math.max(1, Number(p.max_parallel) || 1)) });
  }
</script>

<Modal title="Preferences" {onClose}>
  <div class="prefs">
    <label class="num">
      Theme
      <select value={theme} onchange={(e) => onTheme(e.target.value)}>
        <option value="system">System</option>
        <option value="light">Light (WinSCP-style)</option>
        <option value="dark">Dark</option>
      </select>
    </label>
    <label><input type="checkbox" bind:checked={p.show_hidden} /> Show hidden files (dotfiles)</label>
    <label><input type="checkbox" bind:checked={p.show_owner_group} /> Show Owner/Group columns (remote)</label>
    <label><input type="checkbox" bind:checked={p.confirm_delete} /> Confirm before deleting</label>
    <label><input type="checkbox" bind:checked={p.confirm_overwrite} /> Prompt when files already exist</label>
    <label><input type="checkbox" bind:checked={p.atomic_uploads} /> Atomic uploads (temp name + rename)</label>
    <label class="num">
      Parallel transfers
      <input type="number" min="1" max="8" bind:value={p.max_parallel} />
    </label>
  </div>
  <div class="dlg-actions">
    <button onclick={onClose}>Cancel</button>
    <button class="primary" onclick={save}>Save</button>
  </div>
</Modal>

<style>
  .prefs { display: flex; flex-direction: column; gap: 10px; margin-bottom: 14px; font-size: 13px; }
  .prefs label { display: flex; align-items: center; gap: 8px; }
  .num input { width: 60px; font: inherit; padding: 3px 6px; margin-left: auto; }
  .dlg-actions { display: flex; justify-content: flex-end; gap: 8px; }
  .dlg-actions button { padding: 5px 12px; }
  button.primary { border-color: var(--accent, dodgerblue); }
</style>
