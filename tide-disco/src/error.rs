// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the tide-disco library.

// You should have received a copy of the MIT License
// along with the tide-disco library. If not, see <https://mit-license.org/>.

pub use disco_types::error::*;

pub trait ErrorExt {
    fn into_tide_error(self) -> tide::Error;
    fn from_server_error(source: tide::Error) -> Self;
}

impl<E: Error> ErrorExt for E {
    fn into_tide_error(self) -> tide::Error {
        tide::Error::new(self.status(), self)
    }

    fn from_server_error(source: tide::Error) -> Self {
        match source.downcast::<Self>() {
            Ok(err) => err,
            Err(source) => Self::catch_all(source.status().into(), source.to_string()),
        }
    }
}
