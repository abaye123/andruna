// Spike S1: faults that ART turns into Java exceptions through its signal
// handler. A hot method reads a field through a reference that is sometimes
// null (compiled code uses an implicit null check, a fault on address 0+off),
// and a recursion overflows the stack (compiled code probes below the stack
// pointer, a fault in the protected region).
public class Faults {
    int value = 1;

    static int read(Faults f) {
        return f.value;
    }

    static int depth = 0;

    static void recurse() {
        depth++;
        recurse();
    }

    public static void main(String[] args) {
        Faults obj = new Faults();
        long sum = 0;
        int npes = 0;
        long t = System.nanoTime();
        for (int i = 0; i < 2000000; i++) {
            try {
                sum += read((i % 1000) == 999 ? null : obj);
            } catch (NullPointerException e) {
                npes++;
            }
        }
        long ms = (System.nanoTime() - t) / 1000000L;
        System.out.println("null checks: " + npes + " NullPointerException caught, sum " + sum + ", " + ms + " ms");
        try {
            recurse();
        } catch (StackOverflowError e) {
            System.out.println("stack overflow: StackOverflowError caught at depth " + depth);
        }
    }
}
