#include <cstdint>
#include <stdexcept>

// yabridge throws std::system_error once its Windows host has died, and only C++ can catch
// it. `call` is Rust code that calls into the plug-in through C-unwind function pointers.
extern "C" bool solstice_clap_guard(void (*call)(void*), void* ctx) {
    try {
        call(ctx);
        return true;
    } catch (...) {
        return false;
    }
}

// A clap_plugin.process that throws, for the tests.
extern "C" int32_t solstice_clap_throw(const void*, const void*) {
    throw std::runtime_error("host died");
}
