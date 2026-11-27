// The wall clock and the monotonic clock as Java sees them: prints
// System.currentTimeMillis() (gettimeofday in ART), the seconds of
// java.time.Instant.now() (clock_gettime(CLOCK_REALTIME)) and whether
// System.nanoTime() advances. The andruna-app tests compare the first two
// with the Windows system time.
import java.time.Instant;

public class Clock {
    public static void main(String[] args) throws Exception {
        long n0 = System.nanoTime();
        long ms = System.currentTimeMillis();
        long sec = Instant.now().getEpochSecond();
        Thread.sleep(2);
        long n1 = System.nanoTime();
        System.out.println("currentTimeMillis: " + ms);
        System.out.println("Instant.now: " + sec);
        System.out.println("nanoTime advances: " + (n1 > n0));
    }
}
