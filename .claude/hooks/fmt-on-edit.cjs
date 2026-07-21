// PostToolUse hook (Write|Edit): best-effort `cargo fmt` on the workspace a
// just-edited .rs file belongs to. Local-editing convenience only — per-commit
// CI deliberately does NOT gate on cargo fmt (see AGENTS.md), so this never
// blocks and never reports failure back to the model.
let raw = '';
process.stdin.on('data', (c) => (raw += c));
process.stdin.on('end', () => {
  try {
    const { tool_input } = JSON.parse(raw);
    const filePath = (tool_input && tool_input.file_path) || '';
    if (!filePath.toLowerCase().endsWith('.rs')) return;

    const normalized = filePath.replace(/\\/g, '/');
    const workspace = normalized.includes('/apps/linux/gtk/') ? 'apps/linux/gtk' : 'rust';

    require('child_process').spawn('cargo', ['fmt', '--all'], {
      cwd: workspace,
      stdio: 'ignore',
    });
  } catch {
    // best-effort only
  }
});
