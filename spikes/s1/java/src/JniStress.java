// JNI transitions under GC: worker threads call ordinary (not @FastNative)
// native methods of the boot class path in a loop while another thread runs
// the GC and walks every thread's stack. Each GC and stack walk suspends the
// threads or runs a checkpoint on them, so the compiled JNI stubs regularly
// find a thread flag set and take their slow paths (artJniMethodStart before
// the native call, artJniMethodEnd after it), which need the Thread*.
import android.system.Os;

public class JniStress {
    static volatile boolean stop = false;

    public static void main(String[] args) throws Exception {
        int threads = args.length > 0 ? Integer.parseInt(args[0]) : 8;
        long millis = args.length > 1 ? Long.parseLong(args[1]) : 3000L;
        final long[] calls = new long[threads];
        Thread[] workers = new Thread[threads];
        final int pid = Os.getpid();
        for (int i = 0; i < threads; i++) {
            final int n = i;
            workers[i] = new Thread(() -> {
                long c = 0;
                while (!stop) {
                    for (int k = 0; k < 1000; k++) {
                        if (Os.getpid() != pid) {
                            throw new AssertionError("getpid changed");
                        }
                        Os.getuid();
                        c += 2;
                    }
                    // Some garbage, so that the GC has work to do.
                    Object[] junk = new Object[64];
                    for (int k = 0; k < junk.length; k++) {
                        junk[k] = new byte[128];
                    }
                }
                calls[n] = c;
            }, "jni-" + i);
            workers[i].start();
        }
        long gcs = 0;
        long walks = 0;
        long end = System.nanoTime() + millis * 1000000L;
        while (System.nanoTime() < end) {
            System.gc();
            gcs++;
            for (Thread t : workers) {
                t.getStackTrace();
                walks++;
            }
        }
        stop = true;
        long total = 0;
        for (int i = 0; i < threads; i++) {
            workers[i].join();
            total += calls[i];
        }
        System.out.println("jni stress: " + threads + " threads, " + total + " native calls, "
                + gcs + " GCs, " + walks + " stack walks");
    }
}
