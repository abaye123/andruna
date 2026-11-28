package org.andruna.e2e;

/**
 * A Java program inside an installed package, for the end-to-end test: run
 * by andruna-app with the package's base.apk as the class path, it uses the
 * AOT code dex2oat wrote at install time when there is any.
 */
public class Main {
    static int fib(int n) {
        return n < 2 ? n : fib(n - 1) + fib(n - 2);
    }

    public static void main(String[] args) {
        long t0 = System.nanoTime();
        int f = fib(27);
        long ms = (System.nanoTime() - t0) / 1_000_000;
        StringBuilder sb = new StringBuilder();
        for (String a : args) {
            sb.append(' ').append(a);
        }
        System.out.println("andruna-e2e: fib(27)=" + f + " in " + ms + " ms, vm "
                + System.getProperty("java.vm.version") + ", args:" + sb);
        System.exit(f == 196418 ? 0 : 1);
    }
}
