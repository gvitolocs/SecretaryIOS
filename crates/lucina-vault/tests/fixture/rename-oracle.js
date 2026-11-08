// Real Obsidian renames these files, in this order, and reports every note's text afterwards.
// tests/writes.rs does the same renames on its own and must end with the same bytes.
(async () => {
  app.vault.setConfig('alwaysUpdateLinks', true);
  const steps = [
    ['Alpha.md', 'Renamed Alpha.md'],
    ['Folder A/Dup.md', 'Folder C/Moved Dup.md'],
    ['Evidence/pic.png', 'Evidence/photo.png'],
    ['Project/Permanent/Another claim, with a comma and (brackets).md', 'Project/Permanent/Another claim.md'],
    ['Beta.md', 'Folder A/Dup.md'],
  ];
  for (const [from, to] of steps) {
    const f = app.vault.getAbstractFileByPath(from);
    const dir = to.includes('/') ? to.slice(0, to.lastIndexOf('/')) : '';
    if (dir && !app.vault.getAbstractFileByPath(dir)) await app.vault.createFolder(dir);
    await app.fileManager.renameFile(f, to);
    await new Promise(r => setTimeout(r, 1500));
  }
  const out = {};
  for (const f of app.vault.getMarkdownFiles()) out[f.path] = await app.vault.read(f);
  return JSON.stringify({obsidian: electron.ipcRenderer.sendSync('version'), steps, notes: out});
})()
