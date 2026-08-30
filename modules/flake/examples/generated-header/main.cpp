#include <cstdio>

// Resolves through -I to a file the build GENERATES. At scan time it does
// not exist.
#include "version.gen.h"

int main() {
    printf("Hello %s!\n", GENERATED_GREETING);
    return 0;
}
