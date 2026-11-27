package org.andruna.fixture;

import android.app.Activity;
import android.os.Bundle;

public class MainActivity extends Activity {
    static int counter;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        counter = fib(20);
    }

    static int fib(int n) {
        return n < 2 ? n : fib(n - 1) + fib(n - 2);
    }
}
