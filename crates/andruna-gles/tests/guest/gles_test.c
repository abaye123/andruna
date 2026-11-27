// Guest test of andruna-gles, built freestanding (no libc) as Android ELF
// by tools/build_guest.py and run by tests/guest.rs through
// andruna-gles-run. Without bionic the loader calls main and exits with its
// result: 0 when every check passes, otherwise the number of the first
// failed check.
//
// Checks: display and config selection, an ES 3 context on a pbuffer,
// shaders and a draw read back with glReadPixels, eglGetProcAddress,
// Android config attributes, and a window surface on a headless harness
// target (composition swap chain): clear, read back, swap, resize.

#define EGL_NO_PLATFORM_SPECIFIC_TYPES 1
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES3/gl3.h>

typedef struct ANativeWindow ANativeWindow;
ANativeWindow *andruna_window_create(int width, int height, const char *title);
void andruna_window_destroy(ANativeWindow *w);
int andruna_window_resize(ANativeWindow *w, int width, int height);

#define CHECK(n, cond) \
  do {                 \
    if (!(cond)) return (n); \
  } while (0)

static int near(unsigned a, unsigned b) { return a + 2 >= b && b + 2 >= a; }

static int pixel_is(int x, int y, unsigned r, unsigned g, unsigned b) {
  unsigned char p[4] = {0, 0, 0, 0};
  glReadPixels(x, y, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, p);
  return near(p[0], r) && near(p[1], g) && near(p[2], b) && p[3] == 255;
}

static const char *const kVs =
    "#version 300 es\n"
    "in vec2 pos;\n"
    "void main() { gl_Position = vec4(pos, 0.0, 1.0); }\n";
static const char *const kFs =
    "#version 300 es\n"
    "precision mediump float;\n"
    "uniform vec4 color;\n"
    "out vec4 frag;\n"
    "void main() { frag = color; }\n";

static GLuint shader(GLenum type, const char *src) {
  GLuint s = glCreateShader(type);
  glShaderSource(s, 1, &src, 0);
  glCompileShader(s);
  GLint ok = 0;
  glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
  return ok ? s : 0;
}

int main(void) {
  EGLDisplay dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
  CHECK(1, dpy != EGL_NO_DISPLAY);
  EGLint major = 0, minor = 0;
  CHECK(2, eglInitialize(dpy, &major, &minor) && major == 1 && minor >= 4);
  CHECK(3, eglBindAPI(EGL_OPENGL_ES_API));

  const EGLint want[] = {EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
                         EGL_SURFACE_TYPE, EGL_PBUFFER_BIT | EGL_WINDOW_BIT,
                         EGL_RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT, EGL_NONE};
  EGLConfig cfg = 0;
  EGLint n = 0;
  CHECK(4, eglChooseConfig(dpy, want, &cfg, 1, &n) && n == 1);

  // Android attributes.
  EGLint vid = 0;
  CHECK(5, eglGetConfigAttrib(dpy, cfg, EGL_NATIVE_VISUAL_ID, &vid) && vid == 1);
  const EGLint recordable[] = {EGL_RECORDABLE_ANDROID, EGL_TRUE, EGL_NONE};
  n = -1;
  CHECK(6, eglChooseConfig(dpy, recordable, &cfg + 0, 0, &n) && n == 0);

  const EGLint pb_attrs[] = {EGL_WIDTH, 64, EGL_HEIGHT, 64, EGL_NONE};
  EGLSurface pb = eglCreatePbufferSurface(dpy, cfg, pb_attrs);
  CHECK(7, pb != EGL_NO_SURFACE);
  const EGLint ctx_attrs[] = {EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE};
  EGLContext ctx = eglCreateContext(dpy, cfg, EGL_NO_CONTEXT, ctx_attrs);
  CHECK(8, ctx != EGL_NO_CONTEXT);
  CHECK(9, eglMakeCurrent(dpy, pb, pb, ctx));
  CHECK(10, eglGetCurrentContext() == ctx && eglGetCurrentSurface(EGL_DRAW) == pb);

  // Clear to blue, draw a red triangle over the lower-left half.
  glViewport(0, 0, 64, 64);
  glClearColor(0.0f, 0.0f, 1.0f, 1.0f);
  glClear(GL_COLOR_BUFFER_BIT);
  GLuint vs = shader(GL_VERTEX_SHADER, kVs), fs = shader(GL_FRAGMENT_SHADER, kFs);
  CHECK(11, vs && fs);
  GLuint prog = glCreateProgram();
  glAttachShader(prog, vs);
  glAttachShader(prog, fs);
  glBindAttribLocation(prog, 0, "pos");
  glLinkProgram(prog);
  GLint linked = 0;
  glGetProgramiv(prog, GL_LINK_STATUS, &linked);
  CHECK(12, linked);
  glUseProgram(prog);
  glUniform4f(glGetUniformLocation(prog, "color"), 1.0f, 0.0f, 0.0f, 1.0f);
  static const float tri[] = {-1.0f, -1.0f, 1.0f, -1.0f, -1.0f, 1.0f};
  GLuint vbo = 0;
  glGenBuffers(1, &vbo);
  glBindBuffer(GL_ARRAY_BUFFER, vbo);
  glBufferData(GL_ARRAY_BUFFER, sizeof tri, tri, GL_STATIC_DRAW);
  glEnableVertexAttribArray(0);
  glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, 0);
  // Through eglGetProcAddress, as extension loaders call functions.
  PFNGLDRAWARRAYSPROC draw = (PFNGLDRAWARRAYSPROC)eglGetProcAddress("glDrawArrays");
  CHECK(13, draw != 0);
  draw(GL_TRIANGLES, 0, 3);
  CHECK(14, glGetError() == GL_NO_ERROR);
  CHECK(15, pixel_is(8, 8, 255, 0, 0));
  CHECK(16, pixel_is(56, 56, 0, 0, 255));
  // A float argument through the thunk and back.
  GLfloat lw = 0.0f;
  glLineWidth(1.0f);
  glGetFloatv(GL_LINE_WIDTH, &lw);
  CHECK(17, lw == 1.0f);
  CHECK(18, eglGetProcAddress("glNoSuchFunctionANDRUNA") == 0);

  // A window surface on a headless target.
  ANativeWindow *win = andruna_window_create(48, 40, 0);
  CHECK(20, win != 0);
  // Not a window: rejected.
  CHECK(21, eglCreateWindowSurface(dpy, cfg, (EGLNativeWindowType)&n, 0) == EGL_NO_SURFACE &&
                eglGetError() == EGL_BAD_NATIVE_WINDOW);
  EGLSurface ws = eglCreateWindowSurface(dpy, cfg, (EGLNativeWindowType)win, 0);
  CHECK(22, ws != EGL_NO_SURFACE);
  CHECK(23, eglMakeCurrent(dpy, ws, ws, ctx));
  CHECK(24, eglGetCurrentSurface(EGL_DRAW) == ws);
  EGLint w = 0, h = 0;
  CHECK(25, eglQuerySurface(dpy, ws, EGL_WIDTH, &w) && eglQuerySurface(dpy, ws, EGL_HEIGHT, &h) &&
                w == 48 && h == 40);
  glViewport(0, 0, 48, 40);
  glClearColor(0.0f, 1.0f, 0.0f, 1.0f);
  glClear(GL_COLOR_BUFFER_BIT);
  CHECK(26, pixel_is(10, 10, 0, 255, 0));
  CHECK(27, eglSwapInterval(dpy, 0));
  for (int i = 0; i < 3; i++) CHECK(28, eglSwapBuffers(dpy, ws));
  // Resize: the next swap follows the target's size.
  CHECK(29, andruna_window_resize(win, 32, 24) == 0);
  CHECK(30, eglSwapBuffers(dpy, ws));
  CHECK(31, eglQuerySurface(dpy, ws, EGL_WIDTH, &w) && eglQuerySurface(dpy, ws, EGL_HEIGHT, &h) &&
                w == 32 && h == 24);
  CHECK(32, eglGetCurrentSurface(EGL_DRAW) == ws);
  glViewport(0, 0, 32, 24);
  glClearColor(1.0f, 1.0f, 0.0f, 1.0f);
  glClear(GL_COLOR_BUFFER_BIT);
  CHECK(33, pixel_is(31, 23, 255, 255, 0));
  CHECK(34, eglSwapBuffers(dpy, ws));
  CHECK(35, glGetError() == GL_NO_ERROR);

  CHECK(40, eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT));
  CHECK(41, eglDestroySurface(dpy, ws));
  andruna_window_destroy(win);
  CHECK(42, eglDestroySurface(dpy, pb));
  CHECK(43, eglDestroyContext(dpy, ctx));
  CHECK(44, eglTerminate(dpy));
  return 0;
}
