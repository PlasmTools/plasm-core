# Harness repair before baseline results

The first baseline invocation (`eval-reports/resource-breadth-v5-baseline`) failed to compile the new service continuation test: its returned async block borrowed the owned `visits` parameter (E0373). Zero tests executed and no selection results were produced.

The repair moves `visits` into the future while borrowing the shared call counter. No frozen cases, labels, profiles, generator, schema, limits or production policy changed. The failed build log is preserved. The measured baseline uses the fresh `resource-breadth-v5-baseline-r2` directory.
