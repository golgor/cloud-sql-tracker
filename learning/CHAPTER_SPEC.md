# Chapter authoring spec (internal)

This is the contract every chapter file follows so the book reads as one course.
Not a chapter itself. Do not link it from chapters.

## Audience

A competent programmer with **basic Rust** (ownership, `Result`, traits, `match`)
and **zero context** about this project, Cloud SQL, systemd, or D-Bus.

Assume they do **not** know: what a Cloud SQL Auth Proxy is, what a systemd unit
is, what D-Bus is, what ADC is, what `procfs` is, what a "transient unit" means.
Explain each such term **once, in one sentence, on first use**, then use it
freely. Do not explain what a process, a port, or a `Result` is.

## Non-negotiable rules

1. **Never invent behaviour.** Every claim about the code must be checked against
   the file in `src/`. If you cannot verify it, leave it out.
2. **Quote real code.** Snippets are copied verbatim from the repo. Never
   paraphrase code into something that does not compile. Trim with `// ...` when
   long, but never alter surviving lines.
3. **Cite provenance** under every snippet with `<p class="src">src/file.rs:LINE–LINE</p>`.
   Get the real line numbers (`rg -n`).
4. **Big picture before detail**, inside the chapter too: what problem this part
   solves → the shape → the code → the edge cases.
5. **Explain the why**, citing the frozen contract or ADR that decided it. The
   repo's docs are the authority, not your taste.
6. **British/American spelling**: match the repo (American: "behavior" in code
   comments). Prose may use either, be consistent within a chapter.
7. Short sentences. One idea per sentence. Active voice.
8. No em-dash pile-ups, no filler like "it's important to note that".
9. Do **not** modify anything outside your assigned files. `src/` and `docs/`
   are read-only for this task.

## Required skeleton

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>NN. Title — cloud-sql-tracker</title>
<link rel="stylesheet" href="../assets/textbook.css">
</head>
<body>

<p class="crumb"><a href="../index.html">Contents</a> &rsaquo; Part N &rsaquo; Chapter NN</p>

<header class="book-head">
  <p class="kicker">Chapter NN &middot; Part N</p>
  <h1>Title</h1>
  <p class="lede">One or two sentences: what the reader will be able to do or
  explain after this chapter.</p>
</header>

<!-- body: h2 sections, snippets, callouts, tables, diagrams -->

<section class="check">
  <h2>Check yourself</h2>
  <ol>
    <li>Question?
      <details><summary>Answer</summary><p>...</p></details>
    </li>
    <!-- exactly 3 questions -->
  </ol>
</section>

<footer class="book-foot">
  <p><strong>Primary source:</strong> <a href="URL">Title</a> — one line on why
  this source, not a blog post, is the authority here.</p>
  <p>Stuck, or something here felt hand-wavy? Ask your teacher — that is what I
  am for. Corrections welcome; this book should match the code.</p>
</footer>

<nav class="chapter-nav">
  <a href="NN-prev.html">&larr; Previous title</a>
  <a href="../index.html">Contents</a>
  <a href="NN-next.html">Next title &rarr;</a>
</nav>

</body>
</html>
```

First chapter omits the Previous link; last chapter omits Next.

## Available CSS components

Use these; do not write new CSS and do not use inline `style=`.

| Class | Use for |
|-------|---------|
| `.lede` | Chapter opening sentence(s) |
| `.why` | Design rationale. Start with `<span class="label">Why</span>` |
| `.note` | Neutral aside or clarification |
| `.pure` | A point about pure, testable logic |
| `.io` | A point about I/O and the outside world |
| `.trap` | A gotcha that would bite a newcomer |
| `.diagram` | ASCII/box diagram, inside `<div class="diagram">` |
| `.src` | Provenance line under a snippet |
| `.tag tag-pure` / `.tag tag-io` | Inline marker on a module or fn name |
| `table` | Comparisons, truth tables, exit codes |
| `.check` | The retrieval-practice block (required) |

## Retrieval practice ("Check yourself")

Exactly **3** questions per chapter, in `<details>`. These build long-term
retention, so they must require **recall**, not recognition. Ask "why does X
do Y" or "what would break if Z", never "is X true?".

Keep answers **similar in length** across the three, so length is not a clue.

## Cross-linking

Link generously. Relative paths from `learning/chapters/`:

- other chapters: `07-supervisor.html`
- glossary term: `../reference/glossary.html#connection`
- cheat sheet: `../reference/cheatsheet.html`
- repo file on GitHub: `https://github.com/golgor/cloud-sql-tracker/blob/main/src/reconcile.rs`

When you first use a domain term (Connection, Status document, Health state,
Reconcile, Source, Foreign process, Unit, Supervisor, Group), link it to its
glossary anchor. Anchors are the lower-case dash-case term.

## Verify before you finish

```bash
# every chapter must be valid, self-contained HTML with the stylesheet linked
grep -c 'textbook.css' learning/chapters/YOURFILE.html   # must be 1
# no placeholders left
rg -n 'TODO|FIXME|Lorem|XXX' learning/chapters/YOURFILE.html   # must be empty
```

Confirm every `src/...:LINE` you cite really contains what you claim.
