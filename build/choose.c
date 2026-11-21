#include <unistd.h>
#include <stdio.h>
extern int __real_main(int, char **);

int __wrap_main(int argc, char **argv) {
    __builtin_cpu_init();
    if (__builtin_cpu_supports("avx512ifma")) {
        execv("/ifma", argv);
        perror("/ifma");
        return 1;
    }
    return __real_main(argc, argv);
}
