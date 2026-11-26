// Loaded by linker_test with dlopen after start-up: dynamic TLS, a
// constructor and a destructor.
#include <stdio.h>

__thread char plugin_buf[64] = "plugin-tls";
__thread long plugin_value = 5;
int plugin_constructed = 0;

__attribute__((constructor)) static void plugin_ctor(void) { plugin_constructed = 1; }

__attribute__((destructor)) static void plugin_dtor(void) {
  fputs("plugin destructor\n", stdout);
  fflush(stdout);
}

long plugin_tls(void) { return plugin_value++ + (plugin_buf[0] == 'p'); }
