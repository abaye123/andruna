import android.content.res.Configuration;
import android.content.res.Resources;
import android.os.Handler;
import android.os.Looper;
import android.os.Message;
import android.os.SystemClock;
import android.os.SystemProperties;
import android.util.DisplayMetrics;
import android.util.Log;

import java.util.ArrayList;
import java.util.List;

/**
 * The framework basics an app process needs before anything else, run by
 * andruna-app --framework (andruna_app_process, AndroidRuntime::start):
 * Looper, Handler, Message, SystemClock, android.util.Log, SystemProperties
 * and the system Resources from framework-res.apk. Prints one line per check
 * and exits with the number of failed checks.
 */
public class FrameworkBasics {
    private static final String TAG = "FrameworkBasics";
    private static final List<String> failures = new ArrayList<>();

    private static void check(String name, boolean ok, Object detail) {
        System.out.println((ok ? "PASS " : "FAIL ") + name + ": " + detail);
        if (!ok) {
            failures.add(name);
        }
    }

    public static void main(String[] args) throws Exception {
        // SystemClock: monotonic clocks from the natives.
        long up1 = SystemClock.uptimeMillis();
        long el1 = SystemClock.elapsedRealtime();
        long elNanos = SystemClock.elapsedRealtimeNanos();
        check("SystemClock.uptimeMillis", up1 > 0, up1);
        check("SystemClock.elapsedRealtime", el1 >= up1 && elNanos / 1000000 >= el1 - 1, el1);

        // SystemProperties: the read-only area andruna-lx serves (ADR 0019).
        String sdk = SystemProperties.get("ro.build.version.sdk");
        check("SystemProperties.get ro.build.version.sdk", "37".equals(sdk), sdk);
        check("SystemProperties.getInt", SystemProperties.getInt("ro.build.version.sdk", -1) == 37,
                SystemProperties.getInt("ro.build.version.sdk", -1));
        check("SystemProperties.get missing", SystemProperties.get("andruna.test.missing", "dflt")
                .equals("dflt"), "default returned");

        // android.util.Log: to the logd stand-in (andruna-linker's logd.rs).
        int written = Log.println(Log.WARN, TAG, "framework log line from FrameworkBasics");
        check("Log.println", written > 0, written + " bytes");
        check("Log.isLoggable", !Log.isLoggable(TAG, Log.VERBOSE), "VERBOSE is off by default");

        // Looper, Handler, Message: a message delivered on the main looper,
        // one posted with a delay, then the loop quits.
        Looper.prepareMainLooper();
        Looper main = Looper.getMainLooper();
        check("Looper.getMainLooper", main != null && main == Looper.myLooper(), main);
        final long[] delivered = new long[2];
        final int[] what = new int[1];
        Handler handler = new Handler(main) {
            @Override
            public void handleMessage(Message msg) {
                what[0] = msg.what;
                delivered[0] = SystemClock.uptimeMillis();
            }
        };
        long posted = SystemClock.uptimeMillis();
        handler.sendMessage(handler.obtainMessage(42));
        handler.postDelayed(() -> {
            delivered[1] = SystemClock.uptimeMillis();
            main.quitSafely();
        }, 50);
        Looper.loop();
        check("Handler.sendMessage", what[0] == 42, "what=" + what[0]);
        check("Handler.postDelayed", delivered[1] - posted >= 50,
                (delivered[1] - posted) + " ms after posting (50 requested)");

        // Resources: the system resources from /system/framework/framework-res.apk.
        Resources res = Resources.getSystem();
        String ok = res.getString(android.R.string.ok);
        check("Resources.getString(android.R.string.ok)", "OK".equals(ok), ok);
        String cancel = res.getString(android.R.string.cancel);
        check("Resources.getString(android.R.string.cancel)", "Cancel".equals(cancel), cancel);
        int config = res.getInteger(com.android.internal.R.integer.config_shortAnimTime);
        check("Resources.getInteger(config_shortAnimTime)", config > 0, config);
        Configuration c = res.getConfiguration();
        check("Resources.getConfiguration", c != null, c);
        DisplayMetrics dm = res.getDisplayMetrics();
        check("Resources.getDisplayMetrics", dm != null, dm);

        System.out.println(failures.isEmpty() ? "ALL PASSED" : "FAILED: " + failures);
        System.out.flush();
        System.exit(failures.size());
    }
}
