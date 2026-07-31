# Synthetic reference-format fixture

A hand-written, fully synthetic episode in the reference simulator's input format (percent
SoC, wall-clock timestamps, split cars/sessions files, tuple charge rates). It contains no
data from any proprietary source; every value was chosen here to exercise the converter:
two identities, a chained second session with depletion, a bidirectional and a
unidirectional port, a TOU price ladder, and one DSO window.

Used by CI to round-trip `tools/convert_optimus.py` -> simulate -> referee.
