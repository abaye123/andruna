// Spike S1: a small CPU benchmark for the interpreter and, later, the JIT.
// Each part prints its wall time in milliseconds measured with System.nanoTime.
// An optional first argument multiplies the work of the loop, string and sort parts.
import java.util.Arrays;

public class Bench {
    static int fib(int n) {
        return n < 2 ? n : fib(n - 1) + fib(n - 2);
    }

    static long intLoop(int n) {
        long acc = 0;
        for (int i = 0; i < n; i++) {
            acc += (i * 31) ^ (i >>> 3);
            acc -= i % 7;
        }
        return acc;
    }

    static int stringBuild(int n) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < n; i++) {
            sb.append(i).append(',');
        }
        return sb.length();
    }

    static int sortArray(int n) {
        int[] a = new int[n];
        int x = 12345;
        for (int i = 0; i < n; i++) {
            x = x * 1103515245 + 12345;
            a[i] = x >>> 1;
        }
        Arrays.sort(a);
        return a[n / 2];
    }

    static void report(String name, long start, long result) {
        long ms = (System.nanoTime() - start) / 1000000L;
        System.out.println(name + ": " + ms + " ms (result " + result + ")");
    }

    public static void main(String[] args) {
        int scale = args.length > 0 ? Integer.parseInt(args[0]) : 1;
        long total = System.nanoTime();
        long t = System.nanoTime();
        long r = intLoop(10000000 * scale);
        report("int loop", t, r);
        t = System.nanoTime();
        r = fib(27);
        report("fib(27)", t, r);
        t = System.nanoTime();
        r = stringBuild(200000 * scale);
        report("string build", t, r);
        t = System.nanoTime();
        r = sortArray(200000 * scale);
        report("array sort", t, r);
        report("total", total, 0);
    }
}
