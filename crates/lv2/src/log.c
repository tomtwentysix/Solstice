#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>

/* Defined in lib.rs: whether the calling thread is inside the plug-in's run(). */
bool solstice_lv2_on_audio_thread(void);

int solstice_lv2_log_vprintf(void* handle, uint32_t type, const char* fmt, va_list ap) {
    (void)handle;
    (void)type;
    if (solstice_lv2_on_audio_thread()) {
        return 0;
    }
    fputs("solstice: lv2: ", stderr);
    return vfprintf(stderr, fmt, ap);
}

int solstice_lv2_log_printf(void* handle, uint32_t type, const char* fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = solstice_lv2_log_vprintf(handle, type, fmt, ap);
    va_end(ap);
    return n;
}
