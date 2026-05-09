use proc_macro::TokenStream;
use quote::quote;

#[proc_macro_attribute]
pub fn trace(args: TokenStream, item: TokenStream) -> TokenStream {
    let args = proc_macro2::TokenStream::from(args);
    let item = proc_macro2::TokenStream::from(item);

    quote! {
        #[tracing::instrument(#args)]
        #item
    }
    .into()
}
