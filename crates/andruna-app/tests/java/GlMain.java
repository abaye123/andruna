// A Java program that renders with OpenGL ES in an app process, without the
// framework: main loads a JNI library (args[0], a guest path) whose native
// method takes the window the compositor hands to the process, renders
// with EGL and GLES through ANGLE until the compositor closes the window,
// and returns 0 on success (the checks of gles_window.c). Run by
// andruna-app's tests/gl.rs:
//
//   andruna-app --compositor <handles> --classpath gljava.dex GlMain \
//       /data/app/cp0/libgljni.so
//
// With the framework, the same rendering goes through GLSurfaceView or
// TextureView instead (docs/plan/phase3/gl-in-apps.md).
public class GlMain {
    static native int render();

    public static void main(String[] args) {
        long t0 = System.nanoTime();
        System.load(args[0]);
        long t1 = System.nanoTime();
        System.out.println("gljava: library loaded in " + (t1 - t0) / 1000000 + " ms");
        int r = render();
        System.out.println("gljava: render returned " + r);
        System.exit(r);
    }
}
