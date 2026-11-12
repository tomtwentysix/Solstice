#pragma once

unsigned long x11_create_window(int width, int height, const char* title);
void x11_set_fixed_size(unsigned long w, int width, int height);
void x11_map(unsigned long w);
void x11_raise(unsigned long w);
void x11_resize(unsigned long w, int width, int height);
void x11_destroy(unsigned long w);
void x11_pump(void (*closed)(unsigned long));
