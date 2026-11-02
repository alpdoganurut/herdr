# Seams — <slug>

Tracked in the spec dir. Written at freeze (§3.7), handed to inspector-1. A value with no consumer is dropped before freeze. Inspector-1 checks every merge against this table; a seam with no consumer at the tip is a HIGH.

| value (header/field/row/event) | producer lane · file | consumer lane · file | owning test |
|---|---|---|---|
| | | | |
