const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

function write(file, contents) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, contents, 'utf8');
}

test('same-named files from multiple crates get distinct local anchors', (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'dontspeak-coverage-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const html = path.join(root, 'html');
  const report = path.join(root, 'coverage.html');
  const hrefs = [
    'coverage/repo/rust/crates/alpha/src/lib.rs.html',
    'coverage/repo/rust/crates/beta/src/lib.rs.html',
  ];

  write(path.join(html, 'style.css'), '.line { color: green; }');
  write(path.join(html, 'control.js'), 'function toggle() {}');
  write(path.join(html, 'index.html'), `<body>${hrefs.map((href) =>
    `<tr class='light-row'><td><a href='${href}'>lib.rs</a></td></tr>`).join('')}</body>`);
  for (const href of hrefs) {
    write(path.join(html, href), "<body><a href='#L1'>line</a><pre id='L1'>covered</pre></body>");
  }

  const result = spawnSync(process.execPath, [
    path.join(__dirname, 'merge-crate-coverage.js'), html, 'alpha,beta', report,
  ], { encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr);

  const output = fs.readFileSync(report, 'utf8');
  const sectionIds = [...output.matchAll(/<details id="([^"]+)"/g)].map((match) => match[1]);
  assert.equal(sectionIds.length, 2);
  assert.equal(new Set(sectionIds).size, 2);
  for (const id of sectionIds) {
    assert.match(output, new RegExp(`href='#${id}'`));
    assert.match(output, new RegExp(`href='#${id}-L1'`));
    assert.match(output, new RegExp(`id='${id}-L1'`));
  }
  assert.match(output, /crates\/alpha\/src\/lib\.rs/);
  assert.match(output, /crates\/beta\/src\/lib\.rs/);
});
