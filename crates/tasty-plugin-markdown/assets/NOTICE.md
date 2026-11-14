# mermaid.min.js

- **Source**: `https://cdn.jsdelivr.net/npm/mermaid@11.16.1/dist/mermaid.min.js` (npm package
  `mermaid`, version `11.16.1`, tarball `sha512-TQsq6u22fAn3rek5VOubrhKPo1g5hwC3FXUN9hiyupTckcYiGuuKGkNQrKYwGJkXUxZdojwRG46gsSCFZMDp4g==`).
- **License**: MIT. Confirmed directly against the `LICENSE` file at the matching upstream tag
  (`https://github.com/mermaid-js/mermaid/blob/mermaid%4011.16.1/LICENSE`, copyright 2014-2022
  Knut Sveidqvist) and cross-checked against the npm registry's `license` field for the same
  version — both say `MIT`.
- **Fetched once, offline at runtime**: this file is vendored into the repo and loaded via
  `include_str!` at compile time — no network access happens when tasty runs. The CDN URL above
  is only the one-time source it was pulled from, not a runtime dependency (Tasty's
  offline-first principle).
- The bundle embeds a handful of third-party MIT-licensed snippets (e.g. jQuery's event module,
  a Bezier/Runge-Kutta curve generator) as inline comments — those notices are preserved verbatim
  in `mermaid.min.js` itself and are not restated here.
- To update: re-run the same jsdelivr fetch against a newer `@<version>`, re-verify the LICENSE
  at the matching tag, and update the version/hash above.

# highlight.min.js

- **Source**: `https://cdnjs.cloudflare.com/ajax/libs/highlight.js/11.10.0/highlight.min.js`
  (highlight.js version `11.10.0`, the prebuilt "common" bundle — cdnjs is used here instead of
  jsdelivr/npm because the npm package `highlight.js` ships only source-only CommonJS modules
  (`lib/core.js` + per-language `lib/languages/*.js` requiring each other), not a single-file
  browser-ready bundle; cdnjs publishes the same upstream release already built to a UMD/browser
  global (`window.hljs`), which is what `include_str!` vendoring needs).
  `sha512-eb2a2a6eb70b0070d601d4268919471b8fa6d20fc24ff57d006cb169b1bc8fb264f23debdcae9db9eee8aa9891319a9c61ef5633680f71a34dc67976bb1fb7b4`
  (computed locally with `sha512sum` against the fetched file).
- **License**: BSD-3-Clause. Confirmed directly against the `LICENSE` file at the matching
  upstream tag (`https://github.com/highlightjs/highlight.js/blob/11.10.0/LICENSE`, copyright
  2006 Ivan Sagalaev).
- **Fetched once, offline at runtime**: this file is vendored into the repo and loaded via
  `include_str!` at compile time — no network access happens when tasty runs. The CDN URL above
  is only the one-time source it was pulled from, not a runtime dependency (Tasty's
  offline-first principle).
- **Language coverage**: this is highlight.js's "common" bundle (36 languages, incl. common
  aliases) — `bash, c, cpp, csharp, css, diff, go, graphql, ini, java, javascript, json, kotlin,
  less, lua, makefile, markdown, objectivec, perl, php, php-template, plaintext, python,
  python-repl, r, ruby, rust, scss, shell, sql, swift, typescript, vbnet, wasm, xml, yaml`. TOML
  is covered as an alias of the `ini` grammar (`hljs.getLanguage('toml')` resolves via `ini`'s
  `aliases`), not as a standalone language file. This already covers every language tasty's
  fenced-code-block normalization needs to key off, so no additional per-language files are
  vendored.
- Only the JS engine is vendored — no highlight.js CSS theme (e.g. `github.css`/`github-dark.css`)
  is included. Token colors are mapped in `render.rs` to this plugin's own `--md-*` CSS custom
  properties (the same Theme-derived token set every other rendered element already uses) instead
  of a hardcoded GitHub palette, so highlighted code follows the user's active tasty theme rather
  than always rendering GitHub's fixed colors.
- To update: re-run the same cdnjs fetch against a newer `/<version>/highlight.min.js`, re-verify
  the LICENSE at the matching tag, recompute the sha512, and update the version/hash above.

# katex.min.js / katex.min.css / fonts/KaTeX_*.woff2

- **Source**: `https://cdn.jsdelivr.net/npm/katex@0.18.4/dist/katex.min.js` and
  `https://cdn.jsdelivr.net/npm/katex@0.18.4/dist/katex.min.css` (npm package `katex`, version
  `0.18.4`, the latest stable release per `https://data.jsdelivr.com/v1/packages/npm/katex`'s
  `latest` tag at fetch time).
  - `katex.min.js`
    sha512: `c4e62a7a0618e699f6a5d1bdac91eed4cced4b3d79af06afd574b563b71c7cb503f6825c4c88785ce4a8f8f8f906b5af620538e3cb36b355c46688171d038b0b`
    (computed locally with `sha512sum`; also cross-checked against jsdelivr's own published
    per-file hash — `https://data.jsdelivr.com/v1/packages/npm/katex@0.18.4`'s file-tree entry
    for `/dist/katex.min.js`, base64 sha256 `LsWRaUHvQ4PgMU6qvMcSMBsGAB2fto4I11HSuuWieho=` —
    matches a locally recomputed base64-sha256 of the fetched bytes exactly).
  - `katex.min.css`
    sha512: `0a382f21b7d7c21ce3382b4ffb0b9030f189679b84636907678a8f1af8c881f913885767b83985fa8256d81e60963eddece90f3c35fd04cbb88438a8e4c59310`
    (same cross-check method; jsdelivr's published hash for `/dist/katex.min.css` is base64
    sha256 `GAwtd9Q019pR1mJcUKlk1P1v29ubyHlqCgFsMMSZMfs=`, matches exactly).
  - `fonts/KaTeX_*.woff2` (20 files — every font family/weight/style KaTeX 0.18.4 ships, `woff2`
    variant only; `woff`/`ttf` siblings are legacy-browser fallbacks this webview-embedded engine
    (evergreen WebKitGTK/WKWebView/WebView2) doesn't need and were not vendored). Verified against
    jsdelivr's published per-file byte size in the same package file-tree listing — every fetched
    file's size matches the manifest exactly (e.g. `KaTeX_AMS-Regular.woff2`: 28076 bytes both
    sides), which is strong evidence of an uncorrupted fetch without pasting 20 individual hashes
    here.
- **License**: MIT. Confirmed directly against the `LICENSE` file at the matching upstream tag
  (`https://github.com/KaTeX/KaTeX/blob/v0.18.4/LICENSE`, copyright 2013-2020 Khan Academy and
  other contributors) and cross-checked against the npm package's `license` field for the same
  version — both say `MIT`. The vendored fonts are covered by the same repository-wide MIT
  license (KaTeX's `LICENSE` file makes no separate font-specific claim).
- **Fetched once, offline at runtime**: `katex.min.js`/`katex.min.css` are loaded via
  `include_str!` and each font via `include_bytes!` at compile time — no network access happens
  when tasty runs. The CDN URLs above are only the one-time source they were pulled from, not a
  runtime dependency (Tasty's offline-first principle).
- **Font offline delivery — data URI, not relative `file://` paths**: every other vendored asset
  in this plugin (mermaid.js/highlight.js, and the whole rendered markdown document itself) is
  fully self-contained inside the single HTML string handed to the host WebView — there is no
  on-disk "plugin assets directory" the running document could resolve a relative font URL
  against at runtime (`include_str!`/`include_bytes!` bake the bytes into the compiled binary;
  nothing is written back out to disk). The document carries no `<base href>` for a relative URL
  to resolve against either — it was removed once local images stopped needing it (docs/dev-guide/plugin-development.md).
  So `render.rs::katex_css_with_embedded_fonts` rewrites each `@font-face`'s
  `src:` list (originally `url(fonts/<name>.woff2) format("woff2"),url(fonts/<name>.woff)
  format("woff"),url(fonts/<name>.ttf) format("truetype")`) down to a single
  `url(data:font/woff2;base64,<...>) format("woff2")` entry, base64-encoding the vendored
  `include_bytes!` font data directly — consistent with every other asset in this plugin already
  being embedded, not referenced externally. Raw vendored footprint is ~254KB (20 `.woff2` files);
  base64 inflates that by exactly 4/3 inside the generated CSS (see `docs/surfaces/markdown/screens/
  viewer.md`'s math section for the measured total document/binary impact).
- To update: re-run the same jsdelivr fetch against a newer `katex@<version>` (JS, CSS, and every
  file under `dist/fonts/*.woff2`), re-verify the LICENSE at the matching tag, recompute the
  hashes, and update the version/hashes above.
