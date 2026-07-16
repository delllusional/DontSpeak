// Flattens a cargo-llvm-cov HTML report for one or more crates into a single
// self-contained HTML file (inlined CSS/JS, per-file links rewritten to in-page
// <details> anchors) so it can be published through any agent artifact surface
// that requires one file with no external references.
//
// Usage: node merge-crate-coverage.js <htmlDir> <crateName>[,<crateName2>,...] <outFile>
//   htmlDir    the --output-dir passed to `cargo llvm-cov report --html`
//              (contains index.html, style.css, control.js, coverage/**)
//   crateNames comma-separated crate directory names under rust/crates/,
//              e.g. ds-platform or ds-tts,ds-stt
//   outFile    path to write the merged HTML to

const fs = require('fs');
const path = require('path');
const { createHash } = require('crypto');

const [, , htmlDir, crateNamesArg, outFile] = process.argv;
if (!htmlDir || !crateNamesArg || !outFile) {
  console.error('usage: node merge-crate-coverage.js <htmlDir> <crateName>[,<crateName2>,...] <outFile>');
  process.exit(1);
}
const crateNames = crateNamesArg.split(',').map((s) => s.trim()).filter(Boolean);

const style = fs.readFileSync(path.join(htmlDir, 'style.css'), 'utf8');
const control = fs.readFileSync(path.join(htmlDir, 'control.js'), 'utf8');
const indexHtml = fs.readFileSync(path.join(htmlDir, 'index.html'), 'utf8');

function extractBody(html) {
  const m = html.match(/<body>([\s\S]*)<\/body>/);
  if (!m) throw new Error('could not find <body> in report HTML');
  return m[1];
}

// Walk htmlDir/coverage looking for *.html pages whose path passes through
// crates/<crateName>/src/ — this is how cargo-llvm-cov mirrors the source tree,
// regardless of which OS (Windows backslashes vs. Unix slashes) generated it.
const covRoot = path.join(htmlDir, 'coverage');
const needlesPosix = crateNames.map((c) => `crates/${c}/src/`);

function matchedCrate(fullPath) {
  const normalized = fullPath.split(path.sep).join('/');
  return needlesPosix.find((n) => normalized.includes(n));
}

function sourceIdentity(fullPath) {
  const normalized = fullPath.replaceAll('\\', '/');
  const needle = matchedCrate(normalized);
  if (!needle) throw new Error(`path is outside the requested crates: ${fullPath}`);
  return normalized.slice(normalized.indexOf(needle)).replace(/\.html$/, '');
}

function anchorFor(fullPath) {
  const digest = createHash('sha256').update(sourceIdentity(fullPath)).digest('hex').slice(0, 16);
  return `file-${digest}`;
}

function escapeHtml(value) {
  return value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;').replaceAll("'", '&#39;');
}

function prefixFragmentIds(html, prefix) {
  return html
    .replace(/\bid=(['"])([^'"]+)\1/g, (_match, quote, id) => `id=${quote}${prefix}-${id}${quote}`)
    .replace(/\bhref=(['"])#([^'"]+)\1/g, (_match, quote, id) => `href=${quote}#${prefix}-${id}${quote}`);
}

const matches = [];
(function walk(dir) {
  if (!fs.existsSync(dir)) return;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(full);
    else if (entry.name.endsWith('.html') && matchedCrate(full)) {
      matches.push(full);
    }
  }
})(covRoot);

if (matches.length === 0) {
  console.error(`no per-file reports found for crate(s) "${crateNamesArg}" under ${covRoot}`);
  console.error('check the crate name(s) match their directory under rust/crates/');
  process.exit(1);
}
matches.sort();

// Rewrite the summary table: drop rows for files outside the requested crate(s),
// rewrite surviving hrefs from separate pages to in-page anchors.
let indexBody = extractBody(indexHtml);
const rowRe = /<tr class='light-row'>[\s\S]*?<\/tr>/g;
const keptRows = [];
for (const row of indexBody.match(rowRe) || []) {
  const hrefMatch = row.match(/href='([^']+)'/);
  if (!hrefMatch) continue;
  const href = hrefMatch[1];
  if (!matchedCrate(href)) continue;
  keptRows.push(row.replace(/href='[^']+'/, `href='#${anchorFor(href)}'`));
}
const table = `<div class='centered'><table><tr><td class='column-entry-bold'>Filename</td><td class='column-entry-bold'>Function Coverage</td><td class='column-entry-bold'>Line Coverage</td><td class='column-entry-bold'>Region Coverage</td><td class='column-entry-bold'>Branch Coverage</td></tr>${keptRows.join('')}</table></div>`;

let sections = '';
for (const file of matches) {
  const identity = sourceIdentity(file);
  const anchor = anchorFor(file);
  let body = extractBody(fs.readFileSync(file, 'utf8'));
  body = body.replace(/<h2>Coverage Report<\/h2><h4>Created:[^<]*<\/h4>/, '');
  body = prefixFragmentIds(body, anchor);
  sections += `<details id="${anchor}" class="file-section"><summary>${escapeHtml(identity)}</summary>${body}</details>\n`;
}

const title = escapeHtml(crateNames.join(', '));
const out = `<!doctype html><html><head><meta name='viewport' content='width=device-width,initial-scale=1'><meta charset='UTF-8'>
<title>${title} coverage report</title>
<style>
${style}
.file-section { margin: 1em 0; }
.file-section > summary { cursor: pointer; font-weight: bold; font-family: monospace; padding: 6px 10px; background: #0002; border: 1px solid #8888; border-radius: 3px; }
.file-section table { width: 100%; }
</style>
<script>${control}</script>
</head><body>
<h2>Coverage Report — ${title}</h2>
${table}
<h3 style="margin-top:2em">Per-file source</h3>
${sections}
</body></html>`;

fs.writeFileSync(outFile, out);
console.log(`wrote ${out.length} bytes to ${outFile} (${matches.length} files across ${crateNames.length} crate(s))`);
