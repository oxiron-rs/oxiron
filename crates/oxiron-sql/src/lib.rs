//! oxiron-sql —— MyBatis 风格 SQL Mapper 属性宏。
//!
//! 提供 `#[sql]` / `#[select]` / `#[insert]` / `#[update]` / `#[delete]`：
//! 在 **无函数体的 fn 签名** 上标注 SQL，宏生成真正执行查询的函数体，
//! 底层调用 [`oxiron_jdbc`] 的 JDBC 抽象层（不依赖 sqlx）。
//!
//! ```ignore
//! #[select("SELECT id, name, age FROM users WHERE id = #{id}")]
//! async fn find_by_id(conn: &SqliteConnection, id: i64) -> Result<Option<User>>;
//! ```
//!
//! 映射规则：
//! - `#{name}` 占位符按 **参数名** 与 fn 形参一一对应，编译期严格校验；
//! - 生成的代码使用预编译语句按位置绑定（防 SQL 注入），下标从 1 开始；
//! - 返回类型决定执行策略（见 `Fetch`）；
//! - `Result<T>` 简写（省略错误类型）时固定为 `oxiron_jdbc::JdbcError`；
//!   自定义错误类型需实现 `From<oxiron_jdbc::JdbcError>`。
//!
//! 另提供 `#[derive(FromRow)]`：列名按字段名映射，生成 `oxiron_jdbc::FromRow` 实现。

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DataStruct, DeriveInput, Error, Fields, FnArg, GenericArgument, ItemFn,
    LitStr, Pat, PathArguments, ReturnType, Signature, TraitItemFn, Type, Visibility,
};

/// 查询形态：决定生成哪种执行代码。
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// `#[sql]`：按返回类型自动推断
    Auto,
    /// `#[select]`：只读查询
    Select,
    /// `#[insert]` / `#[update]` / `#[delete]`：写操作
    Mutate,
}

/// 由返回类型推断出的执行策略。
enum Fetch {
    /// `Option<T>` → query_optional
    Optional(Type),
    /// `Vec<T>` → query_all
    All(Type),
    /// `T`（单行结构体）→ query_one
    One(Type),
    /// 标量（`#[select]` 的 COUNT 等）→ query_scalar
    Scalar(Type),
    /// 标量（写操作）→ execute，返回受影响行数
    Affected(Type),
    /// `()` → execute，丢弃结果
    Unit,
}

struct MapperFn {
    attrs: Vec<Attribute>,
    vis: Visibility,
    sig: Signature,
}

const SCALARS: [&str; 12] = [
    "u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize", "f32", "f64",
];

/// 通用 SQL 映射：按返回类型推断执行策略。
#[proc_macro_attribute]
pub fn sql(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(attr, item, Kind::Auto)
}

/// 只读查询映射（标量返回走 `query_scalar`）。
#[proc_macro_attribute]
pub fn select(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(attr, item, Kind::Select)
}

/// 插入映射，返回受影响行数。
#[proc_macro_attribute]
pub fn insert(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(attr, item, Kind::Mutate)
}

/// 更新映射，返回受影响行数。
#[proc_macro_attribute]
pub fn update(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(attr, item, Kind::Mutate)
}

/// 删除映射，返回受影响行数。
#[proc_macro_attribute]
pub fn delete(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(attr, item, Kind::Mutate)
}

/// 行 → 结构体自动映射：字段名对应列名（不区分大小写）。
#[proc_macro_derive(FromRow)]
pub fn derive_from_row(input: TokenStream) -> TokenStream {
    derive_from_row_impl(input.into())
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

fn expand(attr: TokenStream, item: TokenStream, kind: Kind) -> TokenStream {
    expand_inner(attr, item, kind)
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

fn expand_inner(attr: TokenStream, item: TokenStream, kind: Kind) -> syn::Result<TokenStream2> {
    // ── 1. 解析 SQL 字符串 ──────────────────────────────────────────
    let sql_lit = syn::parse::<LitStr>(attr).map_err(|_| {
        Error::new(
            Span::call_site(),
            "期望 SQL 字符串字面量，例如 #[select(\"SELECT ... WHERE id = #{id}\")]",
        )
    })?;
    let sql_text = sql_lit.value();

    // ── 2. 解析函数签名（支持 `;` 结尾的 mapper 风格，也支持带空体） ──
    let MapperFn { attrs, vis, sig } = parse_mapper_fn(item)?;
    validate_sig(&sig)?;

    // ── 3. 参数拆分：第一个是连接（JDBC Connection），其余是绑定参数 ──
    let mut inputs = sig.inputs.iter();
    let conn_ident = match inputs.next() {
        Some(FnArg::Typed(pt)) => match &*pt.pat {
            Pat::Ident(pi) => pi.ident.clone(),
            _ => {
                return Err(Error::new(
                    pt.pat.span(),
                    "第一个参数（数据库连接）必须是简单具名参数",
                ))
            }
        },
        _ => {
            return Err(Error::new(
                sig.span(),
                "至少需要一个参数：数据库连接（如 conn: &SqliteConnection）",
            ))
        }
    };

    let mut params: Vec<(syn::Ident, Type)> = Vec::new();
    for arg in inputs {
        match arg {
            FnArg::Typed(pt) => match &*pt.pat {
                Pat::Ident(pi) => {
                    let name = pi.ident.clone();
                    if params.iter().any(|(n, _)| *n == name) {
                        return Err(Error::new(name.span(), format!("参数 `{name}` 重复")));
                    }
                    params.push((name, (*pt.ty).clone()));
                }
                _ => {
                    return Err(Error::new(
                        pt.pat.span(),
                        "绑定参数必须是简单具名参数",
                    ))
                }
            },
            FnArg::Receiver(r) => return Err(Error::new(r.span(), "不支持 self 参数")),
        }
    }

    // ── 4. 扫描 #{name} 占位符并严格校验 ───────────────────────────
    let names = scan_placeholders(&sql_text, sql_lit.span())?;
    if names.is_empty() && !params.is_empty() {
        return Err(Error::new(
            sql_lit.span(),
            format!(
                "SQL 中没有 #{{...}} 占位符，但声明了绑定参数 [{}]。若确无参数请删除多余形参",
                names_list(&params)
            ),
        ));
    }
    for n in &names {
        if !params.iter().any(|(p, _)| p == n.as_str()) {
            return Err(Error::new(
                sql_lit.span(),
                format!(
                    "SQL 中的 `#{{{n}}}` 没有对应的函数参数（可用参数：[{}]）",
                    names_list(&params)
                ),
            ));
        }
    }
    for (p, _) in &params {
        if !names.iter().any(|n| n == p.to_string().as_str()) {
            return Err(Error::new(
                p.span(),
                format!("参数 `{p}` 未在 SQL 的 #{{...}} 占位符中使用"),
            ));
        }
    }

    // 按占位符在 SQL 文本中出现的顺序，生成 1 起始的 JDBC 绑定
    let bind_idents: Vec<syn::Ident> = names
        .iter()
        .map(|n| {
            params
                .iter()
                .find(|(p, _)| p == n.as_str())
                .map(|(p, _)| p.clone())
                .expect("已校验的占位符必有对应参数")
        })
        .collect();

    // ── 5. 返回类型 → 执行策略 ─────────────────────────────────────
    let (out_ty, inner, err_given) = extract_result(&sig.output)?;
    let fetch = classify(&inner, kind)?;

    // ── 6. 代码生成：调用 oxiron_jdbc（SQL 模板由驱动按方言展开占位符） ──
    let bind_stmts = bind_idents.iter().enumerate().map(|(i, ident)| {
        let idx = i + 1;
        quote! {
            ::oxiron_jdbc::PreparedStatement::bind(
                &mut *stmt, #idx, ::oxiron_jdbc::ToValue::to_value(&#ident),
            )?;
        }
    });

    let fetch_expr = match &fetch {
        Fetch::Optional(t) => quote! { ::oxiron_jdbc::query_optional::<_, #t>(&*stmt)? },
        Fetch::All(t) => quote! { ::oxiron_jdbc::query_all::<_, #t>(&*stmt)? },
        Fetch::One(t) => quote! { ::oxiron_jdbc::query_one::<_, #t>(&*stmt)? },
        Fetch::Scalar(t) => quote! { ::oxiron_jdbc::query_scalar::<_, #t>(&*stmt)? },
        Fetch::Affected(t) => quote! {
            ::oxiron_jdbc::PreparedStatement::execute(&*stmt)? as #t
        },
        Fetch::Unit => quote! {{
            ::oxiron_jdbc::PreparedStatement::execute(&*stmt)?;
        }},
    };

    let body = quote! {{
        let mut stmt = ::oxiron_jdbc::Connection::prepare(#conn_ident, #sql_text)?;
        #(#bind_stmts)*
        let __out = #fetch_expr;
        ::core::result::Result::Ok(__out)
    }};

    // 简写 `Result<T>`（未写错误类型）→ 固定为 JdbcError
    let ret = if err_given {
        quote! { -> #out_ty }
    } else {
        quote! { -> ::core::result::Result<#inner, ::oxiron_jdbc::JdbcError> }
    };

    let mut sig2 = sig.clone();
    sig2.output = ReturnType::Default;

    Ok(quote! {
        #(#attrs)*
        #vis #sig2 #ret #body
    })
}

fn derive_from_row_impl(input: TokenStream2) -> syn::Result<TokenStream2> {
    let input: DeriveInput = syn::parse2(input)?;
    let name = &input.ident;

    let fields = match &input.data {
        Data::Struct(DataStruct {
            fields: Fields::Named(f),
            ..
        }) => &f.named,
        _ => {
            return Err(Error::new(
                input.span(),
                "#[derive(FromRow)] 仅支持具名字段结构体",
            ))
        }
    };

    let inits = fields.iter().map(|f| {
        let ident = f.ident.as_ref().expect("具名字段必有标识符");
        let col = ident.to_string();
        quote! { #ident: ::oxiron_jdbc::Row::try_get(row, #col)?, }
    });

    Ok(quote! {
        impl ::oxiron_jdbc::FromRow for #name {
            fn from_row(row: &::oxiron_jdbc::Row) -> ::core::result::Result<Self, ::oxiron_jdbc::JdbcError> {
                ::core::result::Result::Ok(Self { #(#inits)* })
            }
        }
    })
}

/// 解析 mapper 函数：支持 `async fn f(...);`（无函数体）与带空体两种形态。
fn parse_mapper_fn(item: TokenStream) -> syn::Result<MapperFn> {
    let ts: TokenStream2 = item.into();
    if let Ok(f) = syn::parse2::<ItemFn>(ts.clone()) {
        return Ok(MapperFn {
            attrs: f.attrs,
            vis: f.vis,
            sig: f.sig,
        });
    }
    if let Ok(f) = syn::parse2::<TraitItemFn>(ts.clone()) {
        return Ok(MapperFn {
            attrs: f.attrs,
            vis: Visibility::Inherited,
            sig: f.sig,
        });
    }
    Err(Error::new(
        ts.span(),
        "期望一个 fn 签名，例如：async fn find(conn: &SqliteConnection, id: i64) -> Result<Option<User>>;",
    ))
}

fn validate_sig(sig: &Signature) -> syn::Result<()> {
    // 同步/异步签名均支持：JDBC 核心是同步 API，异步 fn 体内直接调用
    if sig.constness.is_some() || sig.unsafety.is_some() || sig.abi.is_some() {
        return Err(Error::new(sig.span(), "mapper 函数不能是 const/unsafe/extern"));
    }
    if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
        return Err(Error::new(sig.span(), "原型阶段暂不支持泛型参数"));
    }
    Ok(())
}

/// 解析返回类型 `Result<Inner, E>`；`Result<Inner>` 简写返回 err_given=false。
fn extract_result(output: &ReturnType) -> syn::Result<(Type, Type, bool)> {
    let full = match output {
        ReturnType::Default => {
            return Err(Error::new(
                output.span(),
                "请显式声明返回类型，如 -> Result<Option<User>>",
            ))
        }
        ReturnType::Type(_, ty) => (**ty).clone(),
    };

    let seg = last_segment(&full).ok_or_else(|| Error::new(full.span(), "返回类型必须是 Result<...>"))?;
    if seg.ident != "Result" {
        return Err(Error::new(full.span(), "返回类型必须是 Result<...>"));
    }

    let args = match &seg.arguments {
        PathArguments::AngleBracketed(a) => &a.args,
        _ => return Err(Error::new(full.span(), "返回类型必须是 Result<...>")),
    };

    let mut tys = args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t.clone()),
        _ => None,
    });

    let inner = tys
        .next()
        .ok_or_else(|| Error::new(full.span(), "Result 需要至少一个类型参数，如 Result<u64>"))?;
    let err_given = tys.next().is_some();

    Ok((full, inner, err_given))
}

/// 返回类型 → 执行策略。
fn classify(inner: &Type, kind: Kind) -> syn::Result<Fetch> {
    // `()`
    if let Type::Tuple(t) = inner {
        if t.elems.is_empty() {
            return match kind {
                Kind::Select => Err(Error::new(
                    inner.span(),
                    "#[select] 的返回类型不能是 ()（查询至少返回 Option<T>/Vec<T>/标量）",
                )),
                _ => Ok(Fetch::Unit),
            };
        }
    }

    if let Type::Path(tp) = inner {
        if let Some(seg) = tp.path.segments.last() {
            let ident = seg.ident.to_string();
            match ident.as_str() {
                "Option" => return Ok(Fetch::Optional(single_generic(&seg.arguments, "Option")?)),
                "Vec" => return Ok(Fetch::All(single_generic(&seg.arguments, "Vec")?)),
                _ => {}
            }
            if SCALARS.contains(&ident.as_str()) {
                return Ok(match kind {
                    Kind::Select => Fetch::Scalar(inner.clone()),
                    _ => Fetch::Affected(inner.clone()),
                });
            }
        }
    }

    match kind {
        Kind::Mutate => Err(Error::new(
            inner.span(),
            "增删改的返回类型应为受影响行数（u64 等标量）或 ()",
        )),
        _ => Ok(Fetch::One(inner.clone())),
    }
}

fn single_generic(args: &PathArguments, what: &str) -> syn::Result<Type> {
    let args = match args {
        PathArguments::AngleBracketed(a) => &a.args,
        _ => return Err(Error::new(args.span(), format!("{what} 需要一个类型参数"))),
    };
    args.iter()
        .find_map(|a| match a {
            GenericArgument::Type(t) => Some(t.clone()),
            _ => None,
        })
        .ok_or_else(|| Error::new(args.span(), format!("{what} 需要一个类型参数")))
}

fn last_segment(ty: &Type) -> Option<&syn::PathSegment> {
    match ty {
        Type::Path(tp) => tp.path.segments.last(),
        _ => None,
    }
}

/// 扫描 `#{name}`，返回占位符名字（按出现顺序）。
fn scan_placeholders(sql: &str, span: Span) -> syn::Result<Vec<String>> {
    let mut names = Vec::new();
    let mut rest = sql;
    while let Some(pos) = rest.find("#{") {
        let after = &rest[pos + 2..];
        match after.find('}') {
            None => return Err(Error::new(span, "SQL 中存在未闭合的 `#{`")),
            Some(end) => {
                let name = &after[..end];
                let valid = !name.is_empty()
                    && name
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_');
                if !valid {
                    return Err(Error::new(
                        span,
                        format!("非法的占位符参数名 `#{{{name}}}`"),
                    ));
                }
                names.push(name.to_string());
                rest = &after[end + 1..];
            }
        }
    }
    Ok(names)
}

fn names_list(params: &[(syn::Ident, Type)]) -> String {
    params
        .iter()
        .map(|(n, _)| n.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
