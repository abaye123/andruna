package andruna.libcoretest;

import java.io.IOException;
import java.io.PrintStream;
import java.lang.reflect.Modifier;
import java.nio.file.Files;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import org.junit.runner.Description;
import org.junit.runner.JUnitCore;
import org.junit.runner.Request;
import org.junit.runner.Result;
import org.junit.runner.Runner;
import org.junit.runner.manipulation.Filter;
import org.junit.runner.manipulation.NoTestsRemainException;
import org.junit.runner.notification.Failure;
import org.junit.runner.notification.RunListener;

/**
 * Runs libcore test classes with JUnit 4 inside ART and reports every test on
 * one machine-readable line on standard output:
 *
 * <pre>
 * ##lt START &lt;class&gt;#&lt;method&gt;
 * ##lt PASS  &lt;class&gt;#&lt;method&gt; &lt;ms&gt;
 * ##lt FAIL  &lt;class&gt;#&lt;method&gt; &lt;ms&gt; &lt;reason&gt;
 * ##lt SKIP  &lt;class&gt;#&lt;method&gt; 0 &lt;reason&gt;
 * ##lt CLASS &lt;class&gt; &lt;status&gt; &lt;reason&gt;
 * ##lt NOEXCLUDE &lt;class&gt;#&lt;method&gt;   (an excluded test started: exit 4)
 * ##lt DONE
 * </pre>
 *
 * The host side (tests/libcore/run.py) parses these lines. A test that
 * crashes or hangs the VM leaves a START without its result; the host then
 * restarts the class with that test excluded.
 *
 * Usage: Main [--exclude &lt;class&gt;#&lt;method&gt;]... [--exclude-file &lt;path&gt;]
 * &lt;class&gt;...  (the file lists one excluded test per line)
 */
public final class Main {
    private static final String TAG = "##lt ";
    private static final int MAX_REASON = 600;

    private Main() {}

    public static void main(String[] args) {
        PrintStream out = System.out;
        Set<String> excluded = new HashSet<>();
        List<String> classes = new ArrayList<>();
        for (int i = 0; i < args.length; i++) {
            if (args[i].equals("--exclude") && i + 1 < args.length) {
                excluded.add(args[++i]);
            } else if (args[i].equals("--exclude-file") && i + 1 < args.length) {
                try {
                    for (String l : Files.readAllLines(Paths.get(args[++i]))) {
                        if (!l.trim().isEmpty()) {
                            excluded.add(l.trim());
                        }
                    }
                } catch (IOException e) {
                    emit(out, "ERROR cannot read " + args[i] + ": " + reason(e));
                    System.exit(3);
                }
            } else {
                classes.add(args[i]);
            }
        }
        for (String name : classes) {
            runClass(out, name, excluded);
        }
        emit(out, "DONE");
        // Tests may leave non-daemon threads behind.
        System.exit(0);
    }

    private static void runClass(PrintStream out, String name, Set<String> excluded) {
        Class<?> c;
        try {
            c = Class.forName(name, false, Main.class.getClassLoader());
        } catch (Throwable t) {
            emit(out, "CLASS " + name + " LOADERROR " + reason(t));
            return;
        }
        if (Modifier.isAbstract(c.getModifiers()) || c.isInterface()) {
            emit(out, "CLASS " + name + " ABSTRACT -");
            return;
        }
        Runner runner = Request.aClass(c).getRunner();
        if (!excluded.isEmpty()) {
            Filter filter = new Filter() {
                @Override
                public boolean shouldRun(Description d) {
                    return !d.isTest() || !excluded.contains(id(d));
                }

                @Override
                public String describe() {
                    return "exclusions";
                }
            };
            try {
                filter.apply(runner);
            } catch (NoTestsRemainException e) {
                emit(out, "CLASS " + name + " RAN 0");
                return;
            }
        }
        JUnitCore core = new JUnitCore();
        core.addListener(new Listener(out, excluded));
        try {
            Result r = core.run(runner);
            emit(out, "CLASS " + name + " RAN " + r.getRunCount());
        } catch (Throwable t) {
            emit(out, "CLASS " + name + " RUNNERERROR " + reason(t));
        }
    }

    static String id(Description d) {
        String m = d.getMethodName();
        // Parameterized test names may contain spaces; ids are one word.
        return (d.getClassName() + "#" + (m == null ? "<class>" : m)).replaceAll("\\s", "_");
    }

    static String reason(Throwable t) {
        StringBuilder sb = new StringBuilder();
        sb.append(t.getClass().getName());
        String msg;
        try {
            msg = t.getMessage();
        } catch (Throwable e) {
            msg = "<getMessage threw " + e.getClass().getName() + ">";
        }
        if (msg != null) {
            sb.append(": ").append(msg);
        }
        StackTraceElement[] st = t.getStackTrace();
        if (st != null && st.length > 0) {
            sb.append(" @ ").append(st[0]);
        }
        // The root cause usually names the real problem.
        Throwable cause = t.getCause();
        for (int depth = 0; cause != null && cause.getCause() != null
                && cause.getCause() != cause && depth < 20; depth++) {
            cause = cause.getCause();
        }
        if (cause != null && cause != t) {
            sb.append(" | root cause ").append(cause.getClass().getName());
            if (cause.getMessage() != null) {
                sb.append(": ").append(cause.getMessage());
            }
        }
        String s = sb.toString().replace('\r', ' ').replace('\n', ' ').replace('\t', ' ');
        return s.length() > MAX_REASON ? s.substring(0, MAX_REASON) + "..." : s;
    }

    static void emit(PrintStream out, String line) {
        out.println(TAG + line);
        out.flush();
    }

    private static final class Listener extends RunListener {
        private final PrintStream out;
        private Failure failure;
        private boolean skipped;
        private String skipReason;
        private long start;

        private final Set<String> excluded;

        Listener(PrintStream out, Set<String> excluded) {
            this.out = out;
            this.excluded = excluded;
        }

        @Override
        public void testStarted(Description d) {
            if (excluded.contains(id(d))) {
                // Some runners (JUnitParams) cannot filter single parameter
                // sets; stop instead of running a test that crashed before.
                emit(out, "NOEXCLUDE " + id(d));
                System.exit(4);
            }
            failure = null;
            skipped = false;
            skipReason = null;
            start = System.nanoTime();
            emit(out, "START " + id(d));
        }

        @Override
        public void testFailure(Failure f) {
            Description d = f.getDescription();
            if (!d.isTest()) {
                // Class-level failure: @BeforeClass, a class rule, or a class
                // JUnit cannot run (initializationError has a method name).
                emit(out, "FAIL " + id(d) + " 0 " + reason(f.getException()));
                printTrace(f.getException());
                return;
            }
            if (failure == null) {
                failure = f;
            }
        }

        @Override
        public void testAssumptionFailure(Failure f) {
            skipped = true;
            skipReason = reason(f.getException());
        }

        @Override
        public void testIgnored(Description d) {
            emit(out, "START " + id(d));
            emit(out, "SKIP " + id(d) + " 0 @Ignore");
        }

        @Override
        public void testFinished(Description d) {
            long ms = (System.nanoTime() - start) / 1_000_000;
            if (failure != null) {
                emit(out, "FAIL " + id(d) + " " + ms + " " + reason(failure.getException()));
                printTrace(failure.getException());
            } else if (skipped) {
                emit(out, "SKIP " + id(d) + " " + ms + " " + skipReason);
            } else {
                emit(out, "PASS " + id(d) + " " + ms);
            }
        }

        private void printTrace(Throwable t) {
            try {
                t.printStackTrace(out);
            } catch (Throwable ignored) {
                // The trace is for the log only.
            }
            out.flush();
        }
    }
}
