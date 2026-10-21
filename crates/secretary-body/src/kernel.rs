macro_rules! verified_kernel {
    ($(pub fn $name:ident($($arg:ident: $ty:ty),*) -> ($ret:ident: $rty:ty)
       requires {$($pre:tt)*} ensures {$($post:tt)*} proof {$($proof:tt)*} body {$($body:tt)*})*) => {
        $(pub fn $name($($arg: $ty),*) -> $rty { $($body)* })*
    };
}
include!("kernel_shared.rs");
