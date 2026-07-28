---
name: asterline-brainstorm
description: Run Asterline brainstorm mode with structured, extractable idea cards, judgment-free generation waves, private ranked voting, and evidence-preserving synthesis. Use for `/mode brainstorm`, brainstorm generation/voting/synthesis prompts, or when customizing an Asterline deployment's brainstorming method and card content.
---

# Asterline Brainstorm

Follow the phase named in the runtime prompt. Use the user's language for all
human-readable fields.

## Generate

Suspend judgment throughout SEED, BUILD, and STRETCH waves. Do not critique,
rank, reject, vote, choose a winner, or discuss feasibility during these waves.
Prefer quantity, variety, and atomic ideas.

- SEED: generate independent directions and at least one deliberately wild idea.
- BUILD: include NEW, BUILD, COMBINE, or MUTATE operations. Cite prior candidate
  IDs in `sources` when deriving an idea.
- STRETCH: invert assumptions, remove constraints, borrow cross-domain
  analogies, and bridge previously separate directions.

Emit exactly one control line for every idea and no alternative card format:

```text
@@brainstorm_card {"title":"Short distinct title","proposal":"What to do and why it may help","mechanism":"How it works concretely","operation":"NEW","sources":["R1-A#2"]}
```

Card schema:

- `title`, `proposal`, and `mechanism` are required non-empty strings.
- `operation` is required. Use `SEED`, `NEW`, `BUILD`, `COMBINE`, `MUTATE`,
  `INVERT`, `REMOVE_CONSTRAINT`, `ANALOGY`, or `BRIDGE`.
- `sources` is required and is an array. Use `[]` for independent ideas; use
  canonical candidate IDs supplied by Asterline for derived ideas.
- Keep one proposal per card. Do not put Markdown fences around live control
  lines. Asterline assigns the candidate ID after parsing the card.

Do not repeat cards as prose. Asterline renders valid card envelopes for the
user and constructs the canonical IdeaSet from their fields.

## Vote

Read the complete canonical IdeaSet, then independently rank the requested
number of candidate IDs against the original topic. Use relevance, novelty,
feasibility, expected leverage, and testability. Do not imitate another
participant's ballot.

End with exactly one ballot line:

```text
@@brainstorm_vote {"ranked":["R2-B#3","R1-A#1"],"summary":"Short rationale"}
```

Only vote for IDs present in the supplied IdeaSet. Do not invent or renumber
candidates.

## Synthesize

Treat Asterline's deterministic tally as authoritative. Produce a ranked table,
explain agreement and disagreement, retain useful minority ideas, recommend one
primary and one backup, and define the smallest concrete experiment for the
primary recommendation. Never invent votes or silently break a tie.

## Customize a Deployment

Edit this workspace file to specialize the brainstorm method, vocabulary,
domain constraints, evaluation criteria, or synthesis output. Preserve the
`@@brainstorm_card` and `@@brainstorm_vote` schemas so Asterline can extract
cards and ballots. Asterline creates this file only when it is missing and does
not overwrite deployment-local edits.
