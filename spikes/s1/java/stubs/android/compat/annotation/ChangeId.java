// Compile-only stub written for spike S1 (see build_boot.py).
// It declares just what libcore and ICU use; it is not packaged.
package android.compat.annotation;

import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;

@Retention(RetentionPolicy.CLASS)
@Target({ElementType.FIELD, ElementType.PARAMETER})
public @interface ChangeId {
}
