use nom::{self, IResult};
use std::str;
use proto::{Address, AttributeValue, Envelope, MailboxDatum};
use proto::{RequestId, Response, ResponseCode, Status};

fn crlf(c: u8) -> bool {
    c == b'\r' || c == b'\n'
}

fn list_wildcards(c: u8) -> bool {
    c == b'%' || c == b'*'
}

fn quoted_specials(c: u8) -> bool {
    c == b'"' || c == b'\\'
}

fn resp_specials(c: u8) -> bool {
    c == b']'
}

fn atom_specials(c: u8) -> bool {
    c == b'(' || c == b')' || c == b'{' || c == b' ' || c < 32 ||
    list_wildcards(c) || quoted_specials(c) || resp_specials(c)
}

fn atom_char(c: u8) -> bool {
    !atom_specials(c)
}

fn astring_char(c: u8) -> bool {
    atom_char(c) || resp_specials(c)
}

fn tag_char(c: u8) -> bool {
    c != b'+' && astring_char(c)
}

// Ideally this should use nom's `escaped` macro, but it suffers from broken
// type inference unless compiled with the verbose-errors feature enabled.
fn quoted_data(i: &[u8]) -> IResult<&[u8], &str> {
    let mut escape = false;
    let mut len = 0;
    for c in i {
        if *c == b'"' && !escape {
            break;
        }
        len += 1;
        if *c == b'\\' && !escape {
            escape = true
        } else if escape {
            escape = false;
        }
    }
    IResult::Done(&i[len..], str::from_utf8(&i[..len]).unwrap())
}

named!(quoted<&str>, do_parse!(
    tag_s!("\"") >>
    data: quoted_data >>
    tag_s!("\"") >>
    (data)
));

named!(literal<&str>, do_parse!(
    tag_s!("{") >>
    len: number >>
    tag_s!("}") >>
    tag_s!("\r\n") >>
    data: take!(len) >>
    (str::from_utf8(data).unwrap())
));

named!(string<&str>, alt!(quoted | literal));

named!(status_ok<Status>, map!(tag_no_case!("OK"),
    |s| Status::Ok
));
named!(status_no<Status>, map!(tag_no_case!("NO"),
    |s| Status::No
));
named!(status_bad<Status>, map!(tag_no_case!("BAD"),
    |s| Status::Bad
));
named!(status_preauth<Status>, map!(tag_no_case!("PREAUTH"),
    |s| Status::PreAuth
));
named!(status_bye<Status>, map!(tag_no_case!("BYE"),
    |s| Status::Bye
));

named!(status<Status>, alt!(
    status_ok |
    status_no |
    status_bad |
    status_preauth |
    status_bye
));

named!(number<u32>, map!(nom::digit,
    |s| str::parse(str::from_utf8(s).unwrap()).unwrap()
));

named!(number_64<u64>, map!(nom::digit,
    |s| str::parse(str::from_utf8(s).unwrap()).unwrap()
));

named!(text<&str>, map!(take_till_s!(crlf),
    |s| str::from_utf8(s).unwrap()
));

named!(atom<&str>, map!(take_while1_s!(atom_char),
    |s| str::from_utf8(s).unwrap()
));

fn flag_extension(i: &[u8]) -> IResult<&[u8], &str> {
    if i.len() < 1 || i[0] != b'\\' {
        return IResult::Error(nom::ErrorKind::Custom(0));
    }
    let mut last = 0;
    for (idx, c) in i[1..].iter().enumerate() {
        last = idx;
        if !atom_char(*c) {
            break;
        }
    }
    IResult::Done(&i[last + 1..], str::from_utf8(&i[..last + 1]).unwrap())
}

named!(flag<&str>, alt!(flag_extension | atom));

named!(flag_list<Vec<&str>>, do_parse!(
    tag_s!("(") >>
    elements: opt!(do_parse!(
        flag0: flag >>
        flags: many0!(do_parse!(
            tag_s!(" ") >>
            flag: flag >>
            (flag)
        )) >> ({
            let mut res = vec![flag0];
            res.extend(flags);
            res
        })
    )) >>
    tag_s!(")") >> ({
       if elements.is_some() {
           elements.unwrap()
       } else {
           Vec::new()
       }
    })
));

named!(flag_perm<&str>, alt!(
    map!(tag_s!("\\*"), |s| str::from_utf8(s).unwrap()) |
    flag
));

named!(resp_text_code_permanent_flags<ResponseCode>, do_parse!(
    tag_s!("PERMANENTFLAGS (") >>
    elements: dbg_dmp!(opt!(do_parse!(
        flag0: flag_perm >>
        flags: many0!(do_parse!(
            tag_s!(" ") >>
            flag: flag_perm >>
            (flag)
        )) >> ({
            let mut res = vec![flag0];
            res.extend(flags);
            res
        })
    ))) >>
    tag_s!(")") >> ({
        ResponseCode::PermanentFlags(if elements.is_some() {
            elements.unwrap()
        } else {
            Vec::new()
        })
    })
));

named!(resp_text_code_highest_mod_seq<ResponseCode>, dbg_dmp!(do_parse!(
    tag_s!("HIGHESTMODSEQ ") >>
    num: number_64 >>
    (ResponseCode::HighestModSeq(num))
)));

named!(resp_text_code_read_only<ResponseCode>, do_parse!(
    tag_s!("READ-ONLY") >>
    (ResponseCode::ReadOnly)
));

named!(resp_text_code_read_write<ResponseCode>, do_parse!(
    tag_s!("READ-WRITE") >>
    (ResponseCode::ReadWrite)
));

named!(resp_text_code_try_create<ResponseCode>, do_parse!(
    tag_s!("TRYCREATE") >>
    (ResponseCode::TryCreate)
));

named!(resp_text_code_uid_validity<ResponseCode>, do_parse!(
    tag_s!("UIDVALIDITY ") >>
    num: number >>
    (ResponseCode::UidValidity(num))
));

named!(resp_text_code_uid_next<ResponseCode>, do_parse!(
    tag_s!("UIDNEXT ") >>
    num: number >>
    (ResponseCode::UidNext(num))
));

named!(resp_text_code<ResponseCode>, do_parse!(
    tag_s!("[") >>
    coded: alt!(
        resp_text_code_permanent_flags |
        resp_text_code_uid_validity |
        resp_text_code_uid_next |
        resp_text_code_read_only |
        resp_text_code_read_write |
        resp_text_code_try_create |
        resp_text_code_highest_mod_seq
    ) >>
    // Per the spec, the closing tag should be "] ".
    // See `resp_text` for more on why this is done differently.
    tag_s!("]") >>
    (coded)
));

named!(capability<&str>, do_parse!(
    tag_s!(" ") >>
    atom: take_till1_s!(atom_specials) >>
    (str::from_utf8(atom).unwrap())
));

named!(capability_data<Response>, do_parse!(
    tag_s!("CAPABILITY") >>
    capabilities: many1!(capability) >>
    (Response::Capabilities(capabilities))
));

named!(mailbox_data_flags<Response>, do_parse!(
    tag_s!("FLAGS ") >>
    flags: flag_list >>
    (Response::MailboxData(MailboxDatum::Flags(flags)))
));

named!(mailbox_data_exists<Response>, do_parse!(
    num: number >>
    tag_s!(" EXISTS") >>
    (Response::MailboxData(MailboxDatum::Exists(num)))
));

named!(mailbox_data_recent<Response>, do_parse!(
    num: number >>
    tag_s!(" RECENT") >>
    (Response::MailboxData(MailboxDatum::Recent(num)))
));

named!(mailbox_data<Response>, alt!(
    mailbox_data_flags |
    mailbox_data_exists |
    mailbox_data_recent
));

named!(nstring<Option<&str>>, map!(
    alt!(
        map!(tag_s!("NIL"), |s| str::from_utf8(s).unwrap()) |
        string
    ),
    |s| if s == "NIL" { None } else { Some(s) }
));

named!(address<Address>, do_parse!(
    tag_s!("(") >>
    name: nstring >>
    tag_s!(" ") >>
    adl: nstring >>
    tag_s!(" ") >>
    mailbox: nstring >>
    tag_s!(" ") >>
    host: nstring >>
    tag_s!(")") >>
    (Address { name, adl, mailbox, host })
));

named!(opt_addresses<Option<Vec<Address>>>, alt!(
    map!(tag_s!("NIL"), |s| None) |
    do_parse!(
        tag_s!("(") >>
        addrs: many1!(address) >>
        tag_s!(")") >>
        (Some(addrs))
    )
));

named!(msg_att_envelope<AttributeValue>, do_parse!(
    tag_s!("ENVELOPE (") >>
    date: nstring >>
    tag_s!(" ") >>
    subject: nstring >>
    tag_s!(" ") >>
    from: opt_addresses >>
    tag_s!(" ") >>
    sender: opt_addresses >>
    tag_s!(" ") >>
    reply_to: opt_addresses >>
    tag_s!(" ") >>
    to: opt_addresses >>
    tag_s!(" ") >>
    cc: opt_addresses >>
    tag_s!(" ") >>
    bcc: opt_addresses >>
    tag_s!(" ") >>
    in_reply_to: nstring >>
    tag_s!(" ") >>
    message_id: nstring >>
    tag_s!(")") >> ({
        AttributeValue::Envelope(Envelope {
            date, subject, from, sender, reply_to, to, cc, bcc, in_reply_to, message_id
        })
    })
));

named!(msg_att_internal_date<AttributeValue>, do_parse!(
    tag_s!("INTERNALDATE ") >>
    date: nstring >>
    (AttributeValue::InternalDate(date.unwrap()))
));

named!(msg_att_flags<AttributeValue>, do_parse!(
    tag_s!("FLAGS ") >>
    flags: flag_list >>
    (AttributeValue::Flags(flags))
));

named!(msg_att_rfc822<AttributeValue>, do_parse!(
    tag_s!("RFC822 ") >>
    raw: nstring >>
    (AttributeValue::Rfc822(raw))
));

named!(msg_att_rfc822_size<AttributeValue>, do_parse!(
    tag_s!("RFC822.SIZE ") >>
    num: number >>
    (AttributeValue::Rfc822Size(num))
));

named!(msg_att_mod_seq<AttributeValue>, do_parse!(
    tag_s!("MODSEQ (") >>
    num: number_64 >>
    tag_s!(")") >>
    (AttributeValue::ModSeq(num))
));

named!(msg_att_uid<AttributeValue>, do_parse!(
    tag_s!("UID ") >>
    num: number >>
    (AttributeValue::Uid(num))
));

named!(msg_att<AttributeValue>, alt!(
    msg_att_envelope |
    msg_att_internal_date |
    msg_att_flags |
    msg_att_mod_seq |
    msg_att_rfc822 |
    msg_att_rfc822_size |
    msg_att_uid
));

named!(msg_att_list<Vec<AttributeValue>>, do_parse!(
    tag_s!("(") >>
    elements: do_parse!(
        attr0: msg_att >>
        attrs: many0!(do_parse!(
            tag_s!(" ") >>
            attr: msg_att >>
            (attr)
        )) >> ({
            let mut res = vec![attr0];
            res.extend(attrs);
            res
        })
    ) >>
    tag_s!(")") >>
    (elements)
));

named!(message_data_fetch<Response>, do_parse!(
    num: number >>
    tag_s!(" FETCH ") >>
    attrs: msg_att_list >>
    (Response::Fetch(num, attrs))
));

named!(message_data_expunge<Response>, do_parse!(
    num: number >>
    tag_s!(" EXPUNGE") >>
    (Response::Expunge(num))
));

named!(tag<RequestId>, map!(take_while1_s!(tag_char),
    |s| RequestId(str::from_utf8(s).unwrap().to_string())
));

// This is not quite according to spec, which mandates the following:
//     ["[" resp-text-code "]" SP] text
// However, examples in RFC 4551 (Conditional STORE) counteract this by giving
// examples of `resp-text` that do not include the trailing space and text.
named!(resp_text<(Option<ResponseCode>, Option<&str>)>, do_parse!(
    code: opt!(resp_text_code) >>
    text: text >>
    ({
        let res = if text.len() < 1 {
            None
        } else if code.is_some() {
            Some(&text[1..])
        } else {
            Some(text)
        };
        (code, res)
    })
));

named!(response_tagged<Response>, do_parse!(
    tag: tag >>
    tag_s!(" ") >>
    status: status >>
    tag_s!(" ") >>
    text: resp_text >>
    tag_s!("\r\n") >>
    (Response::Done(tag, status, text.0, text.1))
));

named!(resp_cond<Response>, do_parse!(
    status: status >>
    tag_s!(" ") >>
    text: resp_text >>
    (Response::Data(status, text.0, text.1))
));

named!(response_data<Response>, do_parse!(
    tag_s!("* ") >>
    contents: alt!(
        resp_cond |
        mailbox_data |
        message_data_expunge |
        message_data_fetch |
        capability_data
    ) >>
    tag_s!("\r\n") >>
    (contents)
));

named!(response<Response>, alt!(
    response_data |
    response_tagged
));

pub type ParseResult<'a> = IResult<&'a [u8], Response<'a>>;
pub use nom::Needed as Needed;

pub fn parse_response(msg: &[u8]) -> ParseResult {
    response(msg)
}
