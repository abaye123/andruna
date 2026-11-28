// Guest program for GL in an app process, built freestanding (no libc) as
// Android ELF by tools/build_guest.py and run by andruna-app's tests/gl.rs:
// `andruna-app --compositor <handles> --exec gles_window`.
//
// It takes the window the compositor hands to the process
// (andruna_window_create returns the compositor's surface there), renders
// with OpenGL ES 3 through ANGLE at swap interval 1 until the compositor
// closes the window, and follows resizes: the top half of every frame is
// red before the first resize and green after it, the bottom half blue with
// a yellow quad drawn by a shader from an interleaved vertex buffer in its
// middle, so a screenshot shows whether the content is upright and whether
// draws (not only clears) reach the screen. Exit code 0 when the window was
// resized at least once and closed by the compositor, otherwise the number
// of the failed check.

#define EGL_NO_PLATFORM_SPECIFIC_TYPES 1
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES3/gl3.h>

typedef struct ANativeWindow ANativeWindow;
ANativeWindow *andruna_window_create(int width, int height, const char *title);
void andruna_window_destroy(ANativeWindow *w);
int andruna_window_is_open(ANativeWindow *w);

#define CHECK(n, cond) \
  do {                 \
    if (!(cond)) return (n); \
  } while (0)

// About two minutes at 120 Hz: a cap in case the host never closes.
#define MAX_FRAMES 15000

static const char *const kVs =
    "#version 300 es\n"
    "in vec2 pos;\n"
    "in vec3 col;\n"
    "out vec3 v_col;\n"
    "void main() { gl_Position = vec4(pos, 0.0, 1.0); v_col = col; }\n";
static const char *const kFs =
    "#version 300 es\n"
    "precision mediump float;\n"
    "in vec3 v_col;\n"
    "out vec4 frag;\n"
    "void main() { frag = vec4(v_col, 1.0); }\n";

static GLuint shader(GLenum type, const char *src) {
  GLuint s = glCreateShader(type);
  glShaderSource(s, 1, &src, 0);
  glCompileShader(s);
  GLint ok = 0;
  glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
  return ok ? s : 0;
}

// The quad: x -0.5..0.5, y -0.9..-0.3 (the middle of the bottom half),
// position and color interleaved (stride 20, color at offset 8).
static const float kQuad[] = {
    -0.5f, -0.9f, 1.0f, 1.0f, 0.0f, 0.5f, -0.9f, 1.0f, 1.0f, 0.0f, -0.5f, -0.3f, 1.0f, 1.0f, 0.0f,
    0.5f,  -0.9f, 1.0f, 1.0f, 0.0f, 0.5f, -0.3f, 1.0f, 1.0f, 0.0f, -0.5f, -0.3f, 1.0f, 1.0f, 0.0f,
};

int main(int argc, char **argv) {
  (void)argc;
  (void)argv;
  ANativeWindow *win = andruna_window_create(1, 1, "andruna");
  CHECK(1, win != 0);
  EGLDisplay dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
  CHECK(2, dpy != EGL_NO_DISPLAY);
  CHECK(3, eglInitialize(dpy, 0, 0));
  CHECK(4, eglBindAPI(EGL_OPENGL_ES_API));
  const EGLint want[] = {EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
                         EGL_SURFACE_TYPE, EGL_WINDOW_BIT, EGL_RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT,
                         EGL_NONE};
  EGLConfig cfg = 0;
  EGLint n = 0;
  CHECK(5, eglChooseConfig(dpy, want, &cfg, 1, &n) && n == 1);
  EGLSurface ws = eglCreateWindowSurface(dpy, cfg, (EGLNativeWindowType)win, 0);
  CHECK(6, ws != EGL_NO_SURFACE);
  const EGLint ctx_attrs[] = {EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE};
  EGLContext ctx = eglCreateContext(dpy, cfg, EGL_NO_CONTEXT, ctx_attrs);
  CHECK(7, ctx != EGL_NO_CONTEXT);
  CHECK(8, eglMakeCurrent(dpy, ws, ws, ctx));
  CHECK(9, eglSwapInterval(dpy, 1));

  GLuint vs = shader(GL_VERTEX_SHADER, kVs), fs = shader(GL_FRAGMENT_SHADER, kFs);
  CHECK(20, vs && fs);
  GLuint prog = glCreateProgram();
  glAttachShader(prog, vs);
  glAttachShader(prog, fs);
  glBindAttribLocation(prog, 0, "pos");
  glBindAttribLocation(prog, 1, "col");
  glLinkProgram(prog);
  GLint linked = 0;
  glGetProgramiv(prog, GL_LINK_STATUS, &linked);
  CHECK(21, linked);
  GLuint vbo = 0;
  glGenBuffers(1, &vbo);
  glBindBuffer(GL_ARRAY_BUFFER, vbo);
  glBufferData(GL_ARRAY_BUFFER, sizeof kQuad, kQuad, GL_STATIC_DRAW);
  glEnableVertexAttribArray(0);
  glEnableVertexAttribArray(1);
  glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 20, (void *)0);
  glVertexAttribPointer(1, 3, GL_FLOAT, GL_FALSE, 20, (void *)8);

  EGLint lw = 0, lh = 0;
  CHECK(10, eglQuerySurface(dpy, ws, EGL_WIDTH, &lw) && eglQuerySurface(dpy, ws, EGL_HEIGHT, &lh));
  CHECK(11, lw > 1 && lh > 1);
  int resized = 0;
  int frames = 0;
  while (andruna_window_is_open(win) && frames < MAX_FRAMES) {
    EGLint w = 0, h = 0;
    eglQuerySurface(dpy, ws, EGL_WIDTH, &w);
    eglQuerySurface(dpy, ws, EGL_HEIGHT, &h);
    if (w != lw || h != lh) {
      resized++;
      lw = w;
      lh = h;
    }
    glViewport(0, 0, w, h);
    glDisable(GL_SCISSOR_TEST);
    glClearColor(0.0f, 0.0f, 1.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    // OpenGL's y axis points up: rows h/2..h are the top half.
    glEnable(GL_SCISSOR_TEST);
    glScissor(0, h / 2, w, h - h / 2);
    if (resized)
      glClearColor(0.0f, 1.0f, 0.0f, 1.0f);
    else
      glClearColor(1.0f, 0.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glDisable(GL_SCISSOR_TEST);
    glUseProgram(prog);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    CHECK(12, glGetError() == GL_NO_ERROR);
    if (frames < 2) {
      // The draw is in the buffer: the quad's middle (y -0.6) is yellow,
      // left of it blue. Frame 1 is the first after a present, which
      // unbinds a flip-model back buffer from the pipeline.
      unsigned char p[4] = {0, 0, 0, 0};
      glReadPixels(w / 2, h / 5, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, p);
      CHECK(22, p[0] > 240 && p[1] > 240 && p[2] < 16);
      glReadPixels(w / 10, h / 5, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, p);
      CHECK(23, p[0] < 16 && p[1] < 16 && p[2] > 240);
    }
    CHECK(13, eglSwapBuffers(dpy, ws));
    frames++;
  }
  CHECK(14, frames > 0);
  CHECK(15, resized >= 1);
  CHECK(16, !andruna_window_is_open(win));
  eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
  eglDestroySurface(dpy, ws);
  eglDestroyContext(dpy, ctx);
  // Prints the frame statistics with ANDRUNA_GLES_STATS.
  andruna_window_destroy(win);
  return 0;
}
