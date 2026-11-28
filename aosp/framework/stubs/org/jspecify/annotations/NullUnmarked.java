// Compile-only stub: JSpecify (external/jspecify) is not fetched. It declares the
// annotation as the sources use it; it is not packaged.
package org.jspecify.annotations;

import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;

@Retention(RetentionPolicy.RUNTIME)
@Target({ElementType.TYPE_USE, ElementType.TYPE, ElementType.METHOD,
        ElementType.CONSTRUCTOR, ElementType.PACKAGE, ElementType.MODULE})
public @interface NullUnmarked {
}
