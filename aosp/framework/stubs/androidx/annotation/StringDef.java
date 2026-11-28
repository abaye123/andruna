// Compile-only stub: androidx.annotation (prebuilts/sdk) is not fetched. It declares the annotation as the
// framework's sources use it; it is not packaged.
package androidx.annotation;

import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;

@Retention(RetentionPolicy.CLASS)
@Target({ElementType.TYPE, ElementType.FIELD, ElementType.METHOD, ElementType.PARAMETER,
        ElementType.CONSTRUCTOR, ElementType.LOCAL_VARIABLE, ElementType.ANNOTATION_TYPE,
        ElementType.PACKAGE, ElementType.TYPE_USE})
public @interface StringDef {
    String[] value() default {};
    boolean open() default false;
}
