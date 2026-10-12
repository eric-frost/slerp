# Fuzzing regressions

Inputs the fuzzer (`fuzz/`, `tools/fuzz.sh`) found, kept because the damage can't be made by a few byte edits in a test.

- `laz-chunk-point-count.laz`: PDAL's `test/data/copc/1.2-with-color.copc.laz` (BSD, PDAL) as the `models` fuzz target damaged it: a chunk table whose largest chunk claims more points than fit in the address space, which `laz` reserved a buffer for.
