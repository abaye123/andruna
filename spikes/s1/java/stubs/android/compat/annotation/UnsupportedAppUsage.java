// Compile-only stub written for spike S1 (see build_boot.py).
// It declares just what libcore and ICU use; it is not packaged.
package android.compat.annotation;

import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;

@Retention(RetentionPolicy.CLASS)
@Target({ElementType.TYPE, ElementType.FIELD, ElementType.METHOD, ElementType.PARAMETER, ElementType.CONSTRUCTOR, ElementType.LOCAL_VARIABLE, ElementType.ANNOTATION_TYPE, ElementType.PACKAGE})
public @interface UnsupportedAppUsage {
    long trackingBug() default 0;

    int maxTargetSdk() default Integer.MAX_VALUE;

    String publicAlternatives() default "";

    String expectedSignature() default "";

    String implicitMember() default "";

    String overrideSourcePosition() default "";
}
