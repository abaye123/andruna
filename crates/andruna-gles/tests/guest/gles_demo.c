// Demo of andruna-gles: a guest program (Android ELF on the guest bionic)
// that opens a window through the harness API, draws an animated triangle
// on a changing clear color with OpenGL ES 3 and reports frame times.
//
//   gles_demo [frames] [--headless] [--interval N] [--size WxH] [--latency]
//             [--still] [--calls]
//
// --still draws the first frame's picture every frame (orientation
// checks); --calls measures the cost of GL calls after the frames.
//
// --latency alternates black and white frames (for an external latency
// measurement, e.g. a photodiode or a high-speed camera).
//
// Frame times are measured in the guest (CLOCK_MONOTONIC around
// eglSwapBuffers) and by the host (present to display latency from DXGI
// frame statistics, andruna_window_frame_stats).

#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define EGL_NO_PLATFORM_SPECIFIC_TYPES 1
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES3/gl3.h>

typedef struct ANativeWindow ANativeWindow;
ANativeWindow *andruna_window_create(int width, int height, const char *title);
void andruna_window_destroy(ANativeWindow *w);
int andruna_window_is_open(ANativeWindow *w);
struct andruna_frame_stats {
  uint64_t frames;
  double mean_interval_ms;
  double p99_interval_ms;
  double mean_swap_ms;
  double mean_wait_ms;
  double p99_wait_ms;
  uint64_t max_frame_latency;
};
int andruna_window_frame_stats(ANativeWindow *w, struct andruna_frame_stats *out);

static double now_ms(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return t.tv_sec * 1000.0 + t.tv_nsec / 1e6;
}

static int cmp(const void *a, const void *b) {
  double x = *(const double *)a, y = *(const double *)b;
  return x < y ? -1 : x > y;
}

static const char *const kVs =
    "#version 300 es\n"
    "in vec2 pos;\n"
    "in vec3 col;\n"
    "uniform float angle;\n"
    "out vec3 v_col;\n"
    "void main() {\n"
    "  float c = cos(angle), s = sin(angle);\n"
    "  gl_Position = vec4(c * pos.x - s * pos.y, s * pos.x + c * pos.y, 0.0, 1.0);\n"
    "  v_col = col;\n"
    "}\n";
static const char *const kFs =
    "#version 300 es\n"
    "precision mediump float;\n"
    "in vec3 v_col;\n"
    "out vec4 frag;\n"
    "void main() { frag = vec4(v_col, 1.0); }\n";

static GLuint shader(GLenum type, const char *src) {
  GLuint s = glCreateShader(type);
  glShaderSource(s, 1, &src, NULL);
  glCompileShader(s);
  GLint ok = 0;
  glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
  if (!ok) {
    char log[512];
    glGetShaderInfoLog(s, sizeof log, NULL, log);
    fprintf(stderr, "shader: %s\n", log);
    exit(1);
  }
  return s;
}

int main(int argc, char **argv) {
  int frames = 600, headless = 0, latency = 0, still = 0, calls = 0, interval = 1, width = 640, height = 480;
  for (int i = 1; i < argc; i++) {
    if (!strcmp(argv[i], "--headless")) headless = 1;
    else if (!strcmp(argv[i], "--latency")) latency = 1;
    else if (!strcmp(argv[i], "--still")) still = 1;
    else if (!strcmp(argv[i], "--calls")) calls = 1;
    else if (!strcmp(argv[i], "--interval") && i + 1 < argc) interval = atoi(argv[++i]);
    else if (!strcmp(argv[i], "--size") && i + 1 < argc) sscanf(argv[++i], "%dx%d", &width, &height);
    else frames = atoi(argv[i]);
  }
  ANativeWindow *win = andruna_window_create(width, height, headless ? NULL : "Andruna GLES demo");
  if (!win) {
    fprintf(stderr, "cannot open a window\n");
    return 1;
  }
  double t0 = now_ms();
  EGLDisplay dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
  EGLint major, minor;
  if (!eglInitialize(dpy, &major, &minor)) {
    fprintf(stderr, "eglInitialize: %#x\n", eglGetError());
    return 1;
  }
  const EGLint want[] = {EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
                         EGL_SURFACE_TYPE, EGL_WINDOW_BIT, EGL_RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT,
                         EGL_NONE};
  EGLConfig cfg;
  EGLint n = 0;
  if (!eglChooseConfig(dpy, want, &cfg, 1, &n) || n != 1) {
    fprintf(stderr, "no config\n");
    return 1;
  }
  EGLSurface surf = eglCreateWindowSurface(dpy, cfg, (EGLNativeWindowType)win, NULL);
  const EGLint ctx_attrs[] = {EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE};
  EGLContext ctx = eglCreateContext(dpy, cfg, EGL_NO_CONTEXT, ctx_attrs);
  if (surf == EGL_NO_SURFACE || ctx == EGL_NO_CONTEXT || !eglMakeCurrent(dpy, surf, surf, ctx)) {
    fprintf(stderr, "surface/context: %#x\n", eglGetError());
    return 1;
  }
  eglSwapInterval(dpy, interval);
  printf("EGL %d.%d, %s\n%s\n", major, minor, glGetString(GL_VERSION), glGetString(GL_RENDERER));

  GLuint prog = glCreateProgram();
  glAttachShader(prog, shader(GL_VERTEX_SHADER, kVs));
  glAttachShader(prog, shader(GL_FRAGMENT_SHADER, kFs));
  glBindAttribLocation(prog, 0, "pos");
  glBindAttribLocation(prog, 1, "col");
  glLinkProgram(prog);
  glUseProgram(prog);
  GLint angle = glGetUniformLocation(prog, "angle");
  static const float verts[] = {
      0.0f, 0.8f, 1.0f, 0.2f, 0.2f,  -0.7f, -0.5f, 0.2f, 1.0f, 0.2f,  0.7f, -0.5f, 0.2f, 0.2f, 1.0f,
  };
  GLuint vbo;
  glGenBuffers(1, &vbo);
  glBindBuffer(GL_ARRAY_BUFFER, vbo);
  glBufferData(GL_ARRAY_BUFFER, sizeof verts, verts, GL_STATIC_DRAW);
  glEnableVertexAttribArray(0);
  glEnableVertexAttribArray(1);
  glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 20, (void *)0);
  glVertexAttribPointer(1, 3, GL_FLOAT, GL_FALSE, 20, (void *)8);

  double *dt = calloc(frames > 0 ? frames : 1, sizeof(double));
  double *swap = calloc(frames > 0 ? frames : 1, sizeof(double));
  double first_frame = 0, last = now_ms();
  int done = 0;
  for (; done < frames && andruna_window_is_open(win); done++) {
    EGLint w = 0, h = 0;
    eglQuerySurface(dpy, surf, EGL_WIDTH, &w);
    eglQuerySurface(dpy, surf, EGL_HEIGHT, &h);
    glViewport(0, 0, w, h);
    float t = still ? 0.0f : done / 60.0f;
    if (latency) {
      float c = (float)(done & 1);
      glClearColor(c, c, c, 1.0f);
      glClear(GL_COLOR_BUFFER_BIT);
    } else {
      glClearColor(0.5f + 0.5f * sinf(t), 0.3f, 0.5f + 0.5f * cosf(t * 0.7f), 1.0f);
      glClear(GL_COLOR_BUFFER_BIT);
      glUniform1f(angle, t);
      glDrawArrays(GL_TRIANGLES, 0, 3);
    }
    double s0 = now_ms();
    if (!eglSwapBuffers(dpy, surf)) {
      fprintf(stderr, "eglSwapBuffers: %#x\n", eglGetError());
      return 1;
    }
    double s1 = now_ms();
    if (done == 0) first_frame = s1 - t0;
    swap[done] = s1 - s0;
    dt[done] = s1 - last;
    last = s1;
  }
  if (done > 1) {
    // The first interval includes start-up.
    int m = done - 1;
    qsort(dt + 1, m, sizeof(double), cmp);
    double sum = 0, ssum = 0;
    for (int i = 1; i < done; i++) sum += dt[i];
    for (int i = 0; i < done; i++) ssum += swap[i];
    printf("frames %d, first frame %.1f ms after eglGetDisplay\n", done, first_frame);
    printf("frame interval: mean %.3f ms, p50 %.3f, p99 %.3f, max %.3f (%.1f fps)\n", sum / m,
           dt[1 + m / 2], dt[1 + (int)(m * 0.99)], dt[done - 1], 1000.0 * m / sum);
    printf("eglSwapBuffers: mean %.3f ms\n", ssum / done);
  }
  if (calls) {
    // Cost of a call through the stub library and the thunk into ANGLE
    // (glGetError does little work in ANGLE).
    const int n = 1000000;
    double c0 = now_ms();
    unsigned sink = 0;
    for (int i = 0; i < n; i++) sink += glGetError();
    double c1 = now_ms();
    for (int i = 0; i < n; i++) glUniform1f(angle, (float)i);
    double c2 = now_ms();
    printf("calls: glGetError %.1f ns, glUniform1f %.1f ns per call (%u)\n", (c1 - c0) * 1e6 / n,
           (c2 - c1) * 1e6 / n, sink);
  }
  struct andruna_frame_stats st;
  if (andruna_window_frame_stats(win, &st) == 0)
    printf("host: %llu frames, interval mean %.3f p99 %.3f ms, swap %.3f ms, wait after present "
           "mean %.3f p99 %.3f ms (frame latency %llu)\n",
           (unsigned long long)st.frames, st.mean_interval_ms, st.p99_interval_ms, st.mean_swap_ms,
           st.mean_wait_ms, st.p99_wait_ms, (unsigned long long)st.max_frame_latency);
  eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
  eglDestroySurface(dpy, surf);
  eglDestroyContext(dpy, ctx);
  eglTerminate(dpy);
  andruna_window_destroy(win);
  return 0;
}
