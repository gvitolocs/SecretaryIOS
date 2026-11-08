# R8 rules for Secretary · Lucina. Manifest components come from aapt2 --proguard (build/aapt-rules.pro).

# JNI: liblucina_calls.so binds Java_tech_getsecretary_secretary_Native_<name>.
-keep,includedescriptorclasses class tech.getsecretary.secretary.Native {
    native <methods>;
}

# The Kotlin null-check intrinsics only build exception messages; drop them.
-assumenosideeffects class kotlin.jvm.internal.Intrinsics {
    static void checkNotNull(...);
    static void checkNotNullParameter(...);
    static void checkNotNullExpressionValue(...);
    static void checkParameterIsNotNull(...);
    static void checkExpressionValueIsNotNull(...);
    static void checkReturnedValueIsNotNull(...);
    static void checkFieldIsNotNull(...);
}

-allowaccessmodification
-repackageclasses ''
-dontwarn kotlin.**
-dontwarn org.jetbrains.annotations.**
