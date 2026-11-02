# DWARF fixtures

The AXF-named fixtures are ARM ELF **relocatable** objects built from the checked-in C sources with Apple clang 21. Tests do not invoke a compiler. They cover real compiler output, including section relocations, DWARF 4/5 file indexing, DWARF 5 indexed strings/addresses, inlining and optimized location lists. They contain no host absolute source paths.

Regenerate from this directory:

```sh
clang -target arm-none-eabi -mcpu=cortex-m4 -mthumb -gdwarf-4 -O0 -fdebug-compilation-dir=/firmware -c source/main.c -o arm-dwarf4.axf
clang -target arm-none-eabi -mcpu=cortex-m4 -mthumb -gdwarf-5 -O2 -fdebug-compilation-dir=/firmware -c source/main.c -o arm-dwarf5.axf
```
