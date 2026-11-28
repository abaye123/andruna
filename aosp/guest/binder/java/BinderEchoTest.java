/*
 * Java Binder between two andruna-app processes through servicemanager
 * (docs/plan/phase2/binder.md, "Java Binder"). Compiled against
 * framework.jar (android.os.ServiceManager is a platform API) and run by
 * andruna-app as the main class of a process:
 *
 *   BinderEchoTest service   adds "andruna.test.java_echo" (system uid)
 *   BinderEchoTest client N  finds it, checks the caller identity the
 *                            service sees, times N calls, passes a file
 *                            descriptor, gets a death notification when
 *                            the service exits
 *
 * Results are lines on stdout, as in binder_sm_test.cpp, for a Rust test
 * that starts both processes from sysd. Not built yet: it needs
 * framework.jar and libandroid_runtime (with the Binder JNI, which compiles
 * against this libbinder: target binder_jni_check in binder.cmake).
 */

import android.os.Binder;
import android.os.IBinder;
import android.os.Looper;
import android.os.Parcel;
import android.os.ParcelFileDescriptor;
import android.os.RemoteException;
import android.os.ServiceManager;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

public final class BinderEchoTest {
    static final String NAME = "andruna.test.java_echo";
    static final String DESCRIPTOR = "andruna.test.IJavaEcho";
    static final int ECHO = IBinder.FIRST_CALL_TRANSACTION;
    static final int IDENTITY = ECHO + 1;
    static final int READ_FD = ECHO + 2;
    static final int EXIT = ECHO + 3;

    static final class Service extends Binder {
        Service() {
            attachInterface(null, DESCRIPTOR);
        }

        @Override
        protected boolean onTransact(int code, Parcel data, Parcel reply, int flags)
                throws RemoteException {
            switch (code) {
                case ECHO:
                    data.enforceInterface(DESCRIPTOR);
                    reply.writeByteArray(data.createByteArray());
                    return true;
                case IDENTITY:
                    data.enforceInterface(DESCRIPTOR);
                    reply.writeInt(Binder.getCallingPid());
                    reply.writeInt(Binder.getCallingUid());
                    return true;
                case READ_FD:
                    data.enforceInterface(DESCRIPTOR);
                    try (ParcelFileDescriptor pfd = data.readFileDescriptor();
                            FileInputStream in = new FileInputStream(pfd.getFileDescriptor())) {
                        reply.writeString(new String(in.readAllBytes(), StandardCharsets.UTF_8));
                    } catch (IOException e) {
                        throw new RemoteException(e.toString());
                    }
                    return true;
                case EXIT:
                    System.out.println("SERVICE_EXIT");
                    System.exit(0);
                    return true;
                default:
                    return super.onTransact(code, data, reply, flags);
            }
        }
    }

    static void fail(String what) {
        System.out.println("FAIL " + what);
        System.exit(1);
    }

    static void service() {
        Looper.prepareMainLooper();
        ServiceManager.addService(NAME, new Service());
        System.out.println("SERVICE_READY");
        Looper.loop();
    }

    static void client(int iters) throws Exception {
        IBinder b = ServiceManager.waitForService(NAME);
        if (b == null) fail("waitForService");
        System.out.println("LOOKUP ok descriptor=" + b.getInterfaceDescriptor());

        Parcel data = Parcel.obtain();
        Parcel reply = Parcel.obtain();
        data.writeInterfaceToken(DESCRIPTOR);
        b.transact(IDENTITY, data, reply, 0);
        System.out.println("IDENTITY pid=" + reply.readInt() + " uid=" + reply.readInt());
        data.recycle();
        reply.recycle();

        File f = new File(System.getProperty("java.io.tmpdir"), "java_echo.txt");
        try (FileOutputStream out = new FileOutputStream(f)) {
            out.write("written by the client".getBytes(StandardCharsets.UTF_8));
        }
        try (ParcelFileDescriptor pfd =
                ParcelFileDescriptor.open(f, ParcelFileDescriptor.MODE_READ_ONLY)) {
            data = Parcel.obtain();
            reply = Parcel.obtain();
            data.writeInterfaceToken(DESCRIPTOR);
            data.writeFileDescriptor(pfd.getFileDescriptor());
            b.transact(READ_FD, data, reply, 0);
            if (!"written by the client".equals(reply.readString())) fail("READ_FD");
            data.recycle();
            reply.recycle();
        }
        System.out.println("FD_PASSING ok");

        byte[] payload = new byte[100];
        long[] t = new long[iters];
        for (int i = -500; i < iters; i++) {
            data = Parcel.obtain();
            reply = Parcel.obtain();
            data.writeInterfaceToken(DESCRIPTOR);
            data.writeByteArray(payload);
            long start = System.nanoTime();
            b.transact(ECHO, data, reply, 0);
            long end = System.nanoTime();
            if (i >= 0) t[i] = end - start;
            data.recycle();
            reply.recycle();
        }
        Arrays.sort(t);
        System.out.printf("ROUNDTRIP bytes=100 iters=%d p50_us=%.1f p99_us=%.1f%n", iters,
                t[iters / 2] / 1000.0, t[(int) (iters * 0.99)] / 1000.0);

        CountDownLatch died = new CountDownLatch(1);
        b.linkToDeath(died::countDown, 0);
        data = Parcel.obtain();
        data.writeInterfaceToken(DESCRIPTOR);
        b.transact(EXIT, data, null, IBinder.FLAG_ONEWAY);
        data.recycle();
        if (!died.await(20, TimeUnit.SECONDS)) fail("no death notification");
        System.out.println("DEATH ok");
    }

    public static void main(String[] args) throws Exception {
        if (args.length >= 1 && args[0].equals("service")) {
            service();
        } else if (args.length >= 1 && args[0].equals("client")) {
            client(args.length >= 2 ? Integer.parseInt(args[1]) : 2000);
        } else {
            System.err.println("usage: BinderEchoTest service | client [iterations]");
            System.exit(2);
        }
    }
}
