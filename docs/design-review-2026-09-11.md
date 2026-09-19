# Desktop design review — 11 September 2026

Reviewed at `ddb14b93`. Every desktop screen walked in both themes against Apple's Human
Interface Guidelines and the Liquid Glass material rules, then cross-read against
`apps/frontend/src/styles/workspace.css` (1733 lines) and the route components.

The rendered review, with all 31 screenshots and per-finding filters, is
`~/Sync/daisy/ui-review-2026-09-11/index.html` (served with
`python3 -m http.server 3349 --bind 0.0.0.0` from that directory).

## The verdict

The app has a **palette and a set of conventions**, enforced by a strong guardrail suite. It
does not have a **design system**: nothing specifies elevation, material, motion, iconography,
a control inventory, or a layout grid, so each screen answers those questions for itself. The
39 findings below are almost all downstream of that one absence.

What exists today:

- Semantic colour roles in `:root`, flipped wholesale for `[data-theme="dark"]` — this part is good.
- Three type tiers (`--font-ui` Inter, `--font-mono` JetBrains Mono, `--font-display` Archivo)
  with a usage rule per tier.
- A six-step spacing scale (`--space-1`…`--space-6`), `--control-h: 38px`, `--rail-width`,
  `--content-max`, `--library-max`.
- Guardrails in `apps/frontend/src`: `ui-consistency.test.ts` (19 assertions — every button
  carries a class, headings use `.h1/.h2/.h3`, no hand-written font stacks, no hardcoded hex in
  components, every referenced token is declared, one primary per flex row, an inline-appearance
  ratchet at `CEILING = 402`), `css-orphans.test.ts` (every applied class has a rule **and**
  every rule has a consumer), `testid-coverage.test.ts`, `test:webkit`, `test:contrast`.

What is missing:

| Missing | Consequence seen on screen |
| --- | --- |
| Elevation / material tiers | 32 hand-rolled `box-shadow` recipes; popovers invisible against dark |
| A type ramp | 17 font sizes, including 13 / 13.5 and 12 / 12.5 as separate steps |
| A radius ladder | 18 distinct radii, 2px → 999px, non-concentric nesting |
| A motion spec | 6 durations (120–400ms), every button lifting 1px on hover |
| An icon set covering the app | 29 emoji and dingbat literals across 12 components |
| A control inventory | 17 native checkboxes vs 1 styled switch; 27 bare `<select>` |
| A page-header pattern | 5 different header layouts across 9 routes |
| A layout grid and measures | prose at 100–110 characters; ragged control widths per screen |
| A data-colour policy | tag hues and calendar blue read as second and third accents |

## Measured

```
backdrop-filter                     0 occurrences in apps/frontend/src
style={{ … }}                     824 sites (appearance props ratcheted at 402)
<input type="checkbox">            17     .switch  1
<select>                           27
emoji / dingbat UI literals        29 across 12 files
box-shadow declarations            32 in workspace.css
font-size steps                    17 (9 → 28px)
border-radius values               18
transition durations                6 (120/140/160/180/200/400ms)
```

## Findings

Severity is how much it changes how the app reads; effort is the work to fix. Full text,
screenshots and fixes are in the review page.

### Major

| id | finding | effort |
| --- | --- | --- |
| M1 | Everything sits on one plane — no depth model; 32 shadow recipes, none legible on black | hard |
| M2 | Nothing is translucent; chrome is opaque and scroll edges are hard cuts — **the glass decision** | hard |
| M3 | The accent has stopped meaning "primary" (gold fill on every provider row, Action, Send, Pause) | moderate |
| M4 | Two control vocabularies, one of them the system blue (17 native checkboxes vs 1 switch) | easy |
| M5 | Emoji and dingbats doing the work of an icon set, beside the rail's stroked set | moderate |
| M6 | Every route invents its own page header | moderate |
| M7 | 824 inline style objects put layout decisions outside the token system | hard |
| M8 | The library truncates the titles it exists to show (300px column, 92px date gutter) | easy |
| M9 | While recording, the empty notes box outranks the live transcript; REC is one small pill | moderate |
| M10 | Destructive actions at rest and at full weight (Delete per row, Discard on the recorder bar) | easy |

### Moderate

| id | finding | effort |
| --- | --- | --- |
| D1 | No scales — 17 type sizes, 18 radii, 6 durations | moderate |
| D2 | Prose runs past 100 characters in Settings, Search, Analyzer | easy |
| D3 | The summary prints its TL;DR twice (callout + markdown heading) | easy |
| D4 | Three names and three x-positions for the same affordance (Action ▾ / Copy / Copy all) | easy |
| D5 | Empty containers instead of empty states (Chapters, Chat, Calendar, History, Workflows) | moderate |
| D6 | Transcript bubbles fixed at 60% width; speaker name inline; timestamps live-only | moderate |
| D7 | History is a bare HTML table with underlined links and non-mono timestamps | moderate |
| D8 | Calendar rows run full-bleed, dismiss 800px from content, off-palette blue bar | moderate |
| D9 | Settings uses two heading idioms, and the pane's context label is smaller than its children | easy |
| D10 | The record card runs off the bottom of a 1280×800 window; mixed alignment; stray colon | easy |
| D11 | Search explains itself in three lines, repeats each title as its own snippet, double space in placeholder | easy |
| D12 | Row overflow menus sit ~700px from the row content | easy |
| D13 | Two reds in one view (`--record #D7263D` vs `--danger #B23A2A`) | easy |
| D14 | Speaker bubble colours swap meaning between light and dark | easy |
| D15 | 27 native selects keep OS chrome inside a custom palette | hard |
| D16 | Mono doing label work it was ruled out of (participants titles, tag chips) | easy |
| D17 | Nested corners are not concentric (16px card around 6px controls) | moderate |
| D18 | The zoom control is four detached buttons plus a button-shaped readout | easy |
| D19 | Analyzer's field widths disagree (540 vs 640); labels set as headings | easy |

### Minor — four are defects in the stylesheet

| id | finding | where |
| --- | --- | --- |
| N1 | Focus ring still the pre-rebrand indigo `rgba(59,75,155,.15)` on every input | workspace.css:664 |
| N2 | `var(--line-soft))` — stray paren voids the participant row hairline | workspace.css:1113 |
| N3 | `var(--slate, #8a8378)` — dead token, hardcoded off-palette fallback | workspace.css:416 |
| N4 | TL;DR callout hardcodes `rgba(255,193,59,.16)` instead of `--gold-tint` | workspace.css:1183 |
| N5 | The rail carries a permanent scrollbar track — verify per platform | shell |
| N6 | Every button lifts 1px on hover | `.btn:hover` |
| N7 | Default recording title reads `8:29AM - Ad hoc` | recorder |
| N8 | Display repeats "edit default.css for a commented reference" twice on one screen | settings |
| N9 | Reading surfaces set at 13px, smaller than the list titles beside them | summary, markdown |
| N10 | Sticky headings cut content with a hard edge (`.h1--sticky` paints solid `--bg`) | workspace.css:118 |

N1–N4 slipped past the guardrails because `ui-consistency.test.ts` checks hardcoded colour in
**components**, not in the stylesheet. Extending that assertion to `workspace.css` is part of
the work.

## What the system has to define

1. **Elevation and material.** Four tiers — page, surface, raised, overlay — each with a
   background, a border and one shadow, specified per theme. Dark uses lightness steps where
   light uses shadow. No component writes its own shadow.
2. **The glass question (M2).** The HIG-correct adoption is narrow: rail, page-header band,
   toast stack and menus translucent; library list, transcript and summary opaque, because glass
   on content is the anti-pattern. Gated on `prefers-reduced-transparency` and Increase
   Contrast, with a frame-time check on all three platforms — the recording path already avoids
   per-frame paints deliberately (see the rail dot comment in `workspace.css`), and a blurred
   rail behind a live meter is how that comes back. **Owner decision before any work.**
   Precedent: the dark-chrome experiment (`0ec23df`) was reverted (`6a71190`) as too drastic;
   dark chrome does not come back without an explicit ask.
3. **Type ramp.** Six steps (11 / 12 / 13 / 15 / 20 / 26 proposed), each with a line-height and
   a role, replacing the 17 in use. The three-tier font rule stays as it is.
4. **Radius ladder.** 4 / 8 / 12 / 16 / 999, with inner radii derived from the container
   (`calc(outer - pad)`, floored at 4px) so nesting is concentric.
5. **Motion.** Two durations — state and movement — one easing, `prefers-reduced-motion`
   honoured (the recording pulses already are). No hover translation.
6. **Icon set.** Extend the rail's stroked set to mute, discard, pause, close, check, warn,
   edit, chevron; remove every emoji and dingbat literal from chrome.
7. **Control inventory.** One each of: button (primary / secondary / record / danger), switch,
   checkbox, select, combobox, text field, chip, list row, overflow menu, toast, banner, empty
   state, page header, segmented control, tab bar. Each with its states: rest, hover, focus,
   active, disabled, busy.
8. **Layout.** The grid, the measures (prose ≤ 66ch, list and table widths), the page-header
   contract, and the master-detail proportions.
9. **Colour beyond the accent.** A policy for data colour — tag hues and calendar colours are
   user data, not palette, and currently read as competing accents. Plus one red: capture state
   and destructive intent cannot both own it.
10. **Theme parity rules.** Every role defined in both themes, no role swapping its meaning
    between them (D14), and contrast proven by `test:contrast`.

## Constraints the design work inherits

- **Mac, Windows and Linux are equal targets.** Linux renders in webkit2gtk; anything that
  depends on a modern Chromium-only property needs a check there. The captures in the review
  came from headless Chromium, so platform chrome — scrollbars, select popups, emoji faces —
  will differ from the shipped builds.
- **`css-orphans.test.ts` forbids speculative CSS.** Every rule needs a consumer and every
  applied class needs a rule, so a token or utility layer cannot land ahead of the components
  that use it. Land each layer together with its first conversion.
- **The inline-appearance ratchet is a budget, not a ban.** `CEILING = 402`; the test also fails
  when the count drops without the ceiling being lowered, so every conversion pass ends by
  lowering it.
- **Strict CSP, self-hosted fonts.** No remote assets, no CDN, no new resource types without a
  `tauri.conf.json` directive change and a real-build test.
- **Contrast is already known-imperfect by decision:** white on `--sunset`/gold fails AA and is
  deliberately unfixed brand-wide; record red passes. Gold as text uses `--gold-text`.

## Reproducing the audit

The screenshots came from a throwaway wdio spec against the browser-stub rig, deleted after the
run. To regenerate:

1. Write a spec under `apps/frontend/e2e/specs/` that calls `ensureWorld({})`, walks
   `nav.go(route)` for each route plus the settings sections, sets
   `document.documentElement.dataset.theme` to `light` and `dark`, and calls
   `browser.saveScreenshot()` into a directory from `process.env.SHOT_DIR`.
2. `cd apps/frontend && SHOT_DIR=<dir> npx pnpm@10 e2e:browser:spec e2e/specs/<name>.spec.ts`
   — about 45 seconds for 31 shots.
3. Delete the spec. It is a measuring instrument, not a test.

Superseded for the routes: `npm run capture:review -- <dir>` (`apps/frontend/test/capture-review.mjs`)
walks every rail route, the session detail and its tabs in both themes, plus the gallery, and is
committed rather than thrown away. It drives the browser stub, so the settings sub-tabs, the
calendar and the live recorder still want a real build — the stub answers `stub: no backend for
machine_settings`.

## Order for the branch

Revised on 11 September after the owner's read: the foundation leads. The original order put
M1 last because it was filed with M7 and M2 under "architecture", but the three have opposite
dependency shapes. M1 is foundation — every surface that draws a shadow depends on it, so a
page header or an icon set built before the depth model picks from the 32 recipes M1 then
rewrites. M7 is the harvest: the classes 824 inline styles convert *into* come out of the
component inventory, so converting first means naming every class twice. M2 is a material
applied to M1's tiers, and cannot be specified before depth is.

`css-orphans` forbids a rule with no consumer, but it reads **classes**, not custom
properties — so a token layer can land with the conversions of existing rules, which is how
the spine below landed. A new utility *class* still needs its first consumer in the same
commit.

1. **Defects** — N1–N4, N7, N8. No coupling, order-independent. ✅ `a32015e2`
2. **The spine** — elevation and material tiers, the radius ladder, the motion pair, the type
   ramp, and the guardrail extension covering the stylesheets.
   ✅ `d9ec01a0`, `dc79fcac`, `855e408f`, `8aa01c2d`, `86a5a631`
3. **The component inventory** on the spine — M4 switch, M10 danger demotion, M3 accent
   discipline, M5 icon set, M6 page header, M9 recorder balance, M8 library column, D5 empty
   states, and the D-list items that fall out of each. This is the pass that changes how the
   app reads. **M4, M5, M10, M3, M6, M8, M9, D9 and D5 are done**; the rest stand.

   M10's fix was smaller than the pass that carried it. Moving five resting `Delete` buttons
   and the recorder's `Discard` into a row overflow needed a row overflow to move them into,
   and the app had **three** popup-menu surfaces already — the Action flyout, the speaker
   row's hand-rolled kebab, and the transcript context menu — at three paddings, three
   min-widths and three item recipes. A fourth would have been the drift this branch exists to
   stop. So one `.menu` / `.menu__item` surface, one `RowMenu` component, and the placement
   classes (`.action-menu__pop`, `.row-menu__pop`, `.ctx-menu`) reduced to where the menu
   sits. Red now appears in exactly two places: a `.menu__item--danger` inside a menu the
   reader opened, and `.btn--danger-solid` on the confirm. The Storage section's hand-rolled
   type-DELETE panel — a second implementation of `ConfirmDialog`, down to the typed word —
   became the real dialog.

   M3 had to be measured before it could be fixed. A static scan of the JSX cannot see the
   finding the review describes — four identical gold buttons down the provider column are one
   component rendered per row, in a second file — so the count moved into the contrast sweep,
   which already boots every surface in both themes: for each screen, how many filled accents
   are painted and hit-testable at once. It named four crowded surfaces, the three in the
   review's prose plus the Analyzer. The fixes: the provider rows' `Configure` / `Edit` became
   plain `.btn` (the configured state is carried by the green ✓ and the Active pill, which said
   it already), `Clear key` traded an inline danger style for `.btn--danger`, the detail view's
   `Action ▾` lost `.action-menu__btn`'s gold fill so the open tab's own action — `Save notes`,
   `Send` — keeps the accent, and `Workflows` lost `TRIGGER_COLORS` entirely: a gold dot spending
   the accent on a lifecycle state, beside a sentence and a switch that both already said it.
   The count is now a rule at exit 1, and the sweep reaches the live notebook for the first time
   — nothing in the rail leads there, so both harnesses boot a world already recording. 28 → 29
   surfaces, 2,302 elements, all WCAG AA.

   M6 is one `PageHeader` — title, one-line subtitle, an actions slot, an optional filter row —
   and one page shape under it. Every route in the rail now owns its scroll, so the header can
   stay pinned while the body moves beneath it; `.h1--sticky`, the hack that pinned a bare title
   inside a padded column, is gone. The header's two-line rewrite of Search cost nothing: the
   query syntax it used to explain (every word must appear, `"quote"` a phrase, `?` asks a
   question) moved to a hint line under the field it describes, where it is read at the moment
   it is needed. Settings gained a real title band above its nav, which had been carrying an
   `.h1` inside the column.

   The first cut of the page kept two insets — 24px for a pane, 32px for a document — on the
   theory that a title should line up with the content under it. The rendered rule disagreed:
   the title's x and the actions' right edge moved by 16px between Library and Search, which is
   the finding restated. One inset for every page, and a list column inside a pane keeps its own
   denser row gutter, because the header sits above the whole page rather than above that
   column.

   M8 gave the title the first line to itself, and put when, how long and which tags on a meta
   line under it. The 92px date gutter is gone, the column rests at 380px, and a row past two
   tags collapses to two dots and a count. Two rules hold it, both rendered: no title in the
   fixture is clipped at the column's resting width, and every list row owns the pixel a
   pointer aims at first — its centre.

   That second rule came out of a failure. Moving the tag dots to the meta line put the ghost
   suggestion chip in the middle of an untagged row, and the chip stops the click that would
   open the meeting. Three e2e tests went red, all of them reopening a row to make the pane
   re-read; measured in the browser, the centre of that row resolved to the chip. The marks now
   sit at the row's right edge, and the rule fails the build if anything takes the centre back.

   The plant that proved the first rule found a second defect on the way: the column's width
   lived in two places, a CSS default and an inline `gridTemplateColumns`, and the inline one
   always won — so narrowing the stylesheet's number changed nothing. The number lives in the
   stylesheet now; the divider drags it by setting `--library-list-w`.

   M9 rebalanced the recorder. The split defaulted 70% of the window to the pane holding an
   empty notes box and left the arriving words a strip beneath the divider; it now opens at
   42%, and the notes box grows to two thirds of the window while it is being written in and
   settles back when it is not. `ResizableSplit` takes a `boostFraction` for that — it raises
   the top pane without touching the fraction the user dragged, and a drag drops it. Measured
   on the sweep's live surface: transcript 473px against notes 143px at rest, notes 342px while
   writing.

   The recording state itself was one outlined pill in a corner, and the rail's red Record
   button looks the same at rest as it does live, so red in the rail says "start", not
   "running". A recording now paints a 3px `--record` edge across the top of the window, above
   even a dialog (`--z-record`, the ladder's new top step), and steps back to 40% while paused.
   The pill keeps the elapsed clock, which is information; the `RECORDING` eyebrow beside it
   said what the pill already said and is gone, as is the orphaned mic meter under the notes
   box — it sits beside the mic picker it reports on.

   Three rendered rules hold it, each proven by planting: the transcript is at least as tall as
   the notes box at rest, the notes box grows while focused, and a live recording paints the
   edge.

   D9 gave Settings one way to name a group. Twelve sections opened with an `<h2 className="h2">`
   repeating the word already on the nav item and the page title, and under it the groups named
   themselves four ways: `h3.h2` with an inline font size, `h3.h3`, a `.set-group__title` div,
   and a `.advanced-box__title`. Both heading classes resolve to `--t-read`, so the pane's own
   label rendered at the same size as the things inside it and the hierarchy was flat. The
   section heading is deleted — the rail names the section — and every group in the file is now
   a `Group`, whose title is an `<h2 className="set-group__title">`: a real heading, one class,
   one size. Ledes became `.set-group__hint`, the two `.advanced-box` panels became a `Group`
   holding `.set-group__details` / `.set-group__summary`, Display's loose `Field`s became an
   `Appearance` group of `SettingRow`s carrying the machine scope, and `License` moved out of
   About's centred column into a group of its own. Five `style={{ color: 'var(--danger)' }}`
   became `.meta--danger`; the inline-appearance ceiling fell 354 → 340 and the raw-spacing
   ceiling 175 → 171.

   One static rule holds it, proven by planting both halves: every heading in `Settings.tsx`
   carries `set-group__title`, `modal__title` or `tune-panel__engine` — the tuning panel's
   engine sub-heads, a rank below a group title — and `.set-group` is written by hand exactly
   once, in `Group`. Planting a section heading back reported the file and line; planting a
   second hand-built group surface reported the count.

   The e2e suite failed once on the way, at a `<select>` that kept its old value. `selectOption`
   assigned through the value setter, and a `<select>` drops an assignment for an option it has
   not rendered yet — the prompts list arrives asynchronously — so the spec failed one
   assertion later, on a value mismatch that named nothing. The helper now waits for the option
   and says which select never offered it, proven by asking for an id that never renders.

   D5 gave every container something to say while it holds nothing. The review named five —
   Chapters, Chat, Calendar, History, Workflows — and a sweep of the same defect class found
   three more: the library, search and the analyzer. Eight containers, eight idioms: a bare
   `<p style={{ padding: 16, color: 'var(--text-dim)' }}>`, a `.meta` line, a run-on sentence
   carrying three instructions, and in the chat's case an empty box. They are one component
   now — `EmptyState`, a `.empty` block naming what belongs there, one sentence on what puts it
   there, and the step itself where the screen does not already carry it: Sync calendars,
   Create a workflow, New workflow, Record a meeting, Clear the filter. Where the step is
   already on screen the block omits it and points at what is: the chat's input, the chapters
   panel's Extract button, the search box itself.

   The guardrail is the sweep's, because an empty screen is a rendered state no static scan
   reaches. `contrast.mjs` now boots the app twice more — once against a world whose every
   fillable list returns nothing, once against a profile that has never recorded — and walks
   the eight containers in both themes. Each has to paint exactly one `.empty` block, with a
   title, a sentence long enough to be a sentence, and a reachable step; the run prints the
   step each one offers. 29 → 37 surfaces, 2,686 elements, all still WCAG AA — the empty
   screens are the first the sweep has ever measured. Proven by planting: deleting the
   calendar's block reported `empty:calendar paints 0 empty-state block(s)`, and cutting the
   workflows body to "Nothing here." reported `13 characters of body`. `52-empty-states.spec.ts`
   drives the five steps that navigate, so each is known to work from the empty screen rather
   than only to be present.
3b. **The foundation finished** — the leading the type ramp deferred, and the spacing ladder.
   ✅ `8e572080`, `f8c69752`. Leading is three *ratios* (`--lh-text` 1.5 / `--lh-tight` 1.2 /
   `--lh-flush` 1), not one paired to each of the six steps: `--t-ui` sets `.markdown-view p`
   at 1.6 and `.banner-dismiss` at 1, so leading follows what the text does, not how big it is.
   Unitless, so the 248 inline font sizes lead correctly without carrying a paired token.
   Spacing is a 4px ladder *named by size* — `--space-1` was 8px, so adding the 4px step the
   app plainly needs would have renumbered the ladder under every existing call site with no
   build error.

4. **The long conversions** — inline styles into classes (M7, ceiling stepping down each
   pass) and the styled select (D15).
5. **Glass — M2 is decided: yes** (owner, 11 September). Chrome only: rail, page-header band,
   toast stack, menus. Content stays opaque — glass on content is the anti-pattern. Gated on
   `prefers-reduced-transparency` and Increase Contrast, frame-timed on all three platforms.
   It lands after step 3 because the header band is one of the surfaces being frosted, and
   frosting the chrome twice is how a material becomes a pile of patches.

## The simplification pass — 11 September

The owner's instruction: *simplify the number of colours and UI variations.* Measured first,
then cut.

| | Was | Now | What went, and why |
|---|---|---|---|
| `:root` tokens | 64 | 58 | `--focus` (an alias for `--gold` nothing called), `--content-max` and `--library-max` (widths no rule read), the two glows, `--record-down` |
| colour roles | 25 | 22 | The glows said "primary" and "recording" a second time, in a shadow, over a fill that already says it. Record carried two state shades where gold has one. |
| button classes | 11 | 9 | `.btn--stop` was `.btn--primary` renamed — and it sat on the **Pause** button; `.btn--resume` put a second gold beside `.btn--primary` in the same row. One primary per row now. |
| controls | 17 checkboxes, 1 switch | 12 checkboxes, 6 switches | The rule is below. |
| icon families | 2 | 1 | Five colour emoji (🎙 🔇 🗑 ✨ 🩺) among twenty monochrome dingbats. |
| brand serifs | 3 | 1 | See below. |

**The switch rule.** A switch is a setting that applies the moment it is flipped — flipping it
writes. A checkbox is one of several values a form collects and a Save or Continue button
commits; it also carries agreement ("I have read…") and reveal ("Show passphrase"), neither of
which is a setting. Five checkboxes were writing on change — MCP enable, MCP write access, a
workflow's enabled flag, a calendar subscription's, and the manual speech-level override — and
each is a switch now. `Toggle` moved out of `Settings.tsx` to `components/Toggle.tsx` so the
other three files could reach it.

**Three brand serifs, and the exemption that hid them.** The rail's Daisy wordmark is
Cormorant Garamond (bundled) at `--text`. The About page's Smallbricktory lockup was inline
`'Fraunces', Georgia, serif` at `--gold` — Fraunces is not bundled, so it rendered Georgia, a
font nobody chose. iOS drew its own at `#B8860B`. The font guardrail passed all of it: it
carried `if (/^'Fraunces'/.test(v))` with the comment *About-page company wordmark* — the same
shape as the Inter exemption found last session, **an assertion narrower than the claim it
makes.** There is one `--font-brand` now, both lockups use it, and both exemptions are gone.

`--gold` on the light page measures **1.86:1**. `--gold-text` measures 5.24:1 and exists for
exactly this. The lockup was never caught because `test:contrast` drives the browser stub,
which has no backend for the Settings sub-tabs — so About is one of the ten surfaces the
harness cannot reach. That gap is the finding, not the hex.

### Guardrails added this pass

| Rule | Catches | Proven by |
|---|---|---|
| every token declared in `:root` is referenced | a role the system claims to have and does not | found `--content-max` and `--library-max` on its first run |
| a checkbox that writes on change is a switch | the control disagreeing with what it does | planted the MCP checkbox back, watched it fail at its line |
| components draw with glyphs, not colour emoji | a bitmap the platform picks, with no answer to a theme flip | planted 🗑 in Library, watched it fail |

## Density: the one thing the ladder does not cover

189 raw spacing values remain after the conversion, and they are not debt of the same kind as
the nineteen radii were. A 4px ladder covers 54 of 181 declarations; 89 are entirely off it.

```
10px × 37    6px × 34    14px × 15    2px × 12    18px × 7    5px × 8
```

That is a second, denser rhythm on a 2px grid, and it is what gives chips, rows and controls
their tightness. Snapping it onto 4/8/12/16 is not a rename — it is a redesign of how tight the
app reads, and the right place to take it is per component, in context, with the gallery open.
It belongs to step 3, not to a regex over the stylesheet. Ratcheted meanwhile at `CEILING = 189`:
the count may fall, never rise.

## The gallery

`apps/frontend/test/gallery.html`, served off the stub dev server at `/test/gallery.html`
(`?theme=dark`, `?view=split`). It reads the custom properties out of the CSSOM and draws each
one with itself, so it holds no copy of the stylesheet and cannot drift from it. It lives in
`test/` and is left out of the bundle by vite's single `index.html` input — verified against a
build, not assumed.

It is the answer to a gap this review had: `test:contrast` proves 666 elements are legible, and
says nothing about whether the elevation ladder steps or a ramp is a ramp. Those are looked at.

## What the spine found that this review did not

The audit read the stylesheet for hardcoded colour and counted recipes. It did not evaluate
what those recipes resolve to per theme, and three of the four below are only visible on dark.

| | finding |
| --- | --- |
| 1 | Card shadows mix from `--text`, which is `#F6F2EA` on dark. Every `.surface`, `.recorder` and logo mark was **glowing** downward rather than casting — M1's "none legible on black" was half the story. |
| 2 | `.modal` and `.bgfin-toast` painted themselves `--bg`. On dark an overlay was exactly the tone of the page it covered, separated only by a 1px border. |
| 3 | `.btn--working`'s sweep animates **indigo** — `rgba(120,100,180,…)`, the pre-rebrand accent — across every button reporting work in flight. A fifth indigo sat in `Search.tsx`'s selected-person chip. |
| 4 | Five scrims for one job: `.modal-backdrop` plus four component backdrops, in two colours and two alphas, all mixed by hand. |

| 5 | `.h2`, `.h3` and `.rail .nav-item` wrote `"Inter", system-ui, sans-serif` by hand. That is not the `--font-ui` tier, which also carries Avenir Next and Segoe UI — so on a machine without Inter the headings and the body around them resolve to different faces. Windows is where it shows. |

N1–N4 were found by reading. These were found by extending `ui-consistency.test.ts` to the
stylesheets and by working out what each recipe resolves to in each theme — which is the
argument for the guardrail extension being part of the spine rather than a later tidy.

The fifth is worth a second look, because a guardrail was already meant to catch it. The font
rule banned hand-written stacks in components, read only `workspace.css`, and carried
`if (/^"Inter"/.test(v)) continue;` — an exemption matching the exact defect. Same shape as the
colour rule before it covered the stylesheets: **the assertion was narrower than the claim it
was making.** When a guardrail exists and the defect exists anyway, read the guardrail first.

## Guardrails holding the foundation

Each proven by planting the defect it names and watching it fail; a green new test proves
nothing on its own.

| Rule | Catches |
| --- | --- |
| colour roles declared in both themes | a role that keeps its light value on dark |
| no hand-written `line-height` in the three sheets | leading written at the call site |
| no hand-written font stack, all three sheets | the stacks in finding 5 |
| raw spacing ratchet, `CEILING = 189` | density drifting back off the ladder |
| inline appearance ratchet, `CEILING = 402` | unchanged; lower it on every M7 pass |

## The last list, and the material — 12 September

Two findings closed together because the second is what the first was waiting for: the header
band had to stop changing before glass could be applied to it.

**D15 — 27 native `<select>`.** A native select hands its list to the OS, which paints it in
the system palette and the system metrics: a second design inside this one, reached by neither
the theme, the focus ring, nor the menu surface. `components/Select.tsx` draws the list on the
app's own `.menu` surface and keeps the keyboard contract a listbox owes — arrows and Home/End
move, typing jumps, Enter takes, Escape leaves it as it was. Every call site converted; the
nine hand-written pixel widths in Settings became one column width.

The e2e helpers moved with it: `selectOption` opens the list and clicks the option, `tryOption`
is the same without waiting for the value to stick (the app refuses an unentitled provider),
`optionValueByText` and `optionValues` open the list to read it and leave it as they found it.
A picker's chosen value now lives on the trigger as `data-value`, which is what `expectPicked`
reads. Component tests drive the control through `src/test/pick.ts`.

**M2 — the glass decision, applied.** Chrome only: the page-header band, the menus and the
toast stack carry `--glass-*` backgrounds and one blur. The rail carries the background alone —
it is a grid column the page never passes behind, and measured in Chromium its blur moved 13 of
176,000 pixels, all antialias inside one nav icon, against 6.9% of the band's own pixels with a
scrolled page under it. `design-surfaces.json` splits the two lists, `chromeGlass` and
`chromeTint`, and both halves of the gate read them. Content — rows, cards, running text, the
modal — stays opaque. Three settings turn the material off, and each lands on the
same four tokens: Apple's Increase Contrast, Chromium's `prefers-reduced-transparency`, and
macOS Reduce Transparency, which WebKit answers no media query for. That last one reaches CSS
as `data-reduce-transparency` on `<html>`, written from `hardware::reduce_transparency()` —
`NSWorkspace.accessibilityDisplayShouldReduceTransparency`, read at startup and whenever the
window takes focus again, because changing the setting means leaving the app to do it.

Frame time was the open question: the recording path avoids per-frame paints deliberately, and
a blurred rail behind a live meter is how that comes back. Measured on this Mac, glass on
versus off, 90 frames of the live pulse — WebKit 17.0ms / 17.0ms median, Chromium 8.3ms /
8.3ms. Those are the two engines Playwright ships here; Linux WebKitGTK and Windows WebView2
are different builds of them and have not been timed.

### What the rules caught

Compositing translucent grounds in the sweep — a glass layer means the first opaque ancestor is
no longer what the text sits on — immediately failed a surface that had nothing to do with
glass: `.asr-model.is-on` washed the selected row in `--gold-tint`, an alpha wash, so on the
page ground the dim meta text read **4.38:1**. A selection that stays is painted now
(`--gold-selected`), not washed.

The picker rule found `.menu`'s `min-width: 172px` winning over `.select__pop`'s `min-width:
100%` — a 172px list under a 320px control, on seven surfaces.

### Guardrails added this pass

| Rule | Catches | Proven by |
|---|---|---|
| a list of values is the app's own Select | a native `<select>` anywhere under `src/` | planted one in the History table header |
| every picker opens an opaque, stacked list at least as wide as its control, reading at AA | the list that is not the app's menu surface | found the `min-width` collision on its first run |
| glass is chrome only, and every glass token has its way out | a blur on content; an accessibility setting the material ignores | planted a blur on `.surface`; deleted `--glass-blur: none` from the attribute block |
| chrome paints as glass, and stops under Increase Contrast and Reduce Transparency | the material silently dropped, or a setting that no longer reaches it | planted a flat rail, watched both halves fail |
| the pulse keeps its frames behind glass, on both engines | a per-frame paint coming back into the recording path | the numbers above |

The rule this pass replaced: *translucent chrome answers Increase Contrast* demanded the
translucent selector be restated inside the `prefers-contrast: more` block. The material is
routed through tokens instead, so the selector never appears there and the rule fired on
chrome that does turn off — **the assertion narrower than the claim, again**, this time in the
other direction. The rule that replaced it reads the tokens.
