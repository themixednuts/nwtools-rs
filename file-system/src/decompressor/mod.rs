use crate::{
    azcs::{self, is_azcs},
    FileType, FILESYSTEM,
};
use cli::{
    commands::Commands,
    common::{datasheet::DatasheetFormat, objectstream::ObjectStreamFormat},
    ARGS,
};
use dashmap::DashMap;
use datasheet::Datasheet;
use flate2::Decompress;
use object_stream::{from_reader, JSONObjectStream, XMLObjectStream};
use quick_xml::se::Serializer;
use serde::Serialize;
use std::io::{self, Cursor, Read, Write};
use zip::{read::ZipFile, CompressionMethod};

#[derive()]
pub struct Decompressor<'a, 'b> {
    localization: Option<&'a DashMap<String, Option<String>>>,
    zip: &'a mut ZipFile<'b>,
    buf: Vec<u8>,
}

impl<'a, 'b> Decompressor<'a, 'b> {
    /// Creates a new [`Decompressor`].
    pub fn try_new(
        zip: &'a mut ZipFile<'b>,
        localization: Option<&'a DashMap<String, Option<String>>>,
    ) -> io::Result<Self> {
        let size = zip.size() as usize;
        let mut value = Self {
            localization,
            zip,
            buf: Vec::with_capacity(size),
        };
        value.decompress()?;
        Ok(value)
    }
    // pub fn with_buf(
    //     zip: &'a mut ZipFile<'b>,
    //     localization: &'a Option<DashMap<String, Option<String>>>,
    //     buf: &mut R,
    // ) -> Self {
    //     let size = zip.size() as usize;
    //     Self {
    //         localization,
    //         zip,
    //         buf,
    //     }
    // }
    pub fn decompress(&mut self) -> io::Result<()> {
        if self.zip.size() == 0 {
            return Ok(());
        }

        match self.zip.compression() {
            CompressionMethod::Stored => std::io::copy(&mut self.zip, &mut self.buf),
            CompressionMethod::Deflated => {
                let mut bytes = [0; 2];
                self.zip.read_exact(&mut bytes)?;
                if [0x78, 0xda] == bytes {
                    let mut zip = flate2::read::ZlibDecoder::new_with_decompress(
                        Cursor::new(bytes).chain(&mut self.zip),
                        Decompress::new(true),
                    );
                    std::io::copy(&mut zip, &mut self.buf)
                } else {
                    let mut zip =
                        flate2::read::DeflateDecoder::new(Cursor::new(bytes).chain(&mut self.zip));
                    std::io::copy(&mut zip, &mut self.buf)
                }
            }
            #[allow(deprecated)]
            CompressionMethod::Unsupported(15) => {
                let mut compressed = vec![];
                std::io::copy(self.zip, &mut compressed)?;

                oodle_safe::decompress(
                    &compressed,
                    &mut self.buf,
                    None,
                    None,
                    None,
                    Some(oodle_safe::DecodeThreadPhase::All),
                )
                .map(|size| size as u64)
                .map_err(|_| io::Error::other(format!("Error with oodle_safe::decompress.",)))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::Other,
                "CompressionMethod not supported",
            )),
        }?;

        let mut sig = self.buf[..4].try_into().unwrap();
        if is_azcs(&mut sig) {
            let mut tmp = Vec::with_capacity(self.zip.size() as usize);
            {
                let mut slice = &mut self.buf.as_slice();
                let mut reader = azcs::decompress(&mut slice).unwrap();
                std::io::copy(&mut reader, &mut tmp)?;
            }
            self.buf = tmp;
        };
        Ok(())
    }

    pub fn size(&mut self) {}

    pub fn compressed_size(&mut self) {}

    pub fn file_type(&self) -> io::Result<FileType> {
        let sig = &self.buf[..5];
        let _type = match sig {
            [0x04, 0x00, 0x1B, 0x4C, 0x75] => FileType::Luac,
            [0x00, 0x00, 0x00, 0x00, 0x03] => match &ARGS.command {
                Commands::Extract(extract) => {
                    FileType::ObjectStream(&extract.objectstream.objectstream)
                }
            },
            [0x11, 0x00, 0x00, 0x00, _] => match &ARGS.command {
                Commands::Extract(extract) => FileType::Datasheet(&extract.datasheet.datasheet),
            },
            _ => FileType::default(),
        };

        Ok(_type)
    }

    pub fn to_writer<W: Write>(&self, writer: &'_ mut W) -> io::Result<Option<Metadata<'_>>> {
        let file_type = self.file_type()?;
        let mut extra = None;

        let size = match &file_type {
            FileType::Luac => std::io::copy(&mut (&self.buf[2..]), writer),
            FileType::ObjectStream(fmt) => {
                // early return no serialziation
                if **fmt == ObjectStreamFormat::BYTES {
                    std::io::copy(&mut self.buf.as_slice(), writer)?;
                    return Ok(None);
                };
                let hashes = if let Some(fs) = FILESYSTEM.get() {
                    Some(&fs.hashes)
                } else {
                    None
                };

                let Ok(obj_stream) = from_reader(&mut self.buf.as_slice(), hashes) else {
                    std::io::copy(&mut self.buf.as_slice(), writer)?;
                    return Ok(None);
                };
                match fmt {
                    ObjectStreamFormat::XML => {
                        let obj_stream = XMLObjectStream::from(obj_stream);
                        let mut buf = String::new();
                        let mut ser = Serializer::new(&mut buf);
                        ser.indent('\t', 2);
                        obj_stream.serialize(ser).unwrap();
                        std::io::copy(&mut buf.as_bytes(), writer)
                    }
                    ObjectStreamFormat::MINI => {
                        let obj_stream = JSONObjectStream::from(obj_stream);
                        let string = serde_json::to_string(&obj_stream)
                            .expect("couldnt parse object stream to json");
                        std::io::copy(&mut string.as_bytes(), writer)
                    }
                    ObjectStreamFormat::PRETTY => {
                        let obj_stream = JSONObjectStream::from(obj_stream);
                        let string = serde_json::to_string_pretty(&obj_stream)
                            .expect("couldnt parse object stream to json");
                        std::io::copy(&mut string.as_bytes(), writer)
                    }
                    _ => std::io::copy(&mut self.buf.as_slice(), writer),
                }
            }
            FileType::Datasheet(fmt) => {
                let datasheet = Datasheet::try_from(self.buf.to_owned()).unwrap();

                // if **fmt == DatasheetFormat::BYTES {
                //     return Ok((
                //         std::io::copy(&mut sig.chain(reader), writer)?,
                //         file_type,
                //         Some(Metadata::Datasheet(datasheet.to_owned())),
                //     ));
                // };

                extra = Some(Metadata::Datasheet(datasheet.to_owned()));

                match fmt {
                    DatasheetFormat::MINI => {
                        let string = datasheet.to_json_simd(false)?;
                        std::io::copy(&mut string.as_bytes(), writer)
                    }
                    DatasheetFormat::PRETTY => {
                        let string = datasheet.to_json_simd(true)?;
                        std::io::copy(&mut string.as_bytes(), writer)
                    }
                    DatasheetFormat::YAML => {
                        let string = datasheet.to_yaml();
                        std::io::copy(&mut string.as_bytes(), writer)
                    }
                    DatasheetFormat::CSV => {
                        let string = datasheet.to_csv();
                        std::io::copy(&mut string.as_bytes(), writer)
                    }
                    DatasheetFormat::BYTES => std::io::copy(&mut self.buf.as_slice(), writer),
                    DatasheetFormat::XML => todo!(),
                    DatasheetFormat::SQL => {
                        let string = datasheet.to_sql();
                        std::io::copy(&mut string.as_bytes(), writer)
                    }
                }
            }
            _ => std::io::copy(&mut self.buf.as_slice(), writer),
        }?;

        Ok(extra)
    }
}

// pub trait ZipFileExt {
//     fn decompress(
//         &mut self,
//         buf: &mut impl Write,
//     ) -> std::io::Result<(u64, FileType, Option<Metadata>)>;
// }

// impl ZipFileExt for ZipFile<'_> {
//     fn decompress(
//         &mut self,
//         buf: &mut impl Write,
//     ) -> std::io::Result<(u64, FileType, Option<Metadata>)> {
//         decompress_zip(self, buf)
//     }
// }

// pub fn to_writer<'a>(
//     mut reader: impl Read + Unpin,
//     buf: &'a mut impl Write,
//     localization: Option<&'a DashMap<String, Option<String>>>,
// ) -> io::Result<(u64, FileType, Option<Metadata<'a>>)> {
//     let mut sig = [0; 4];
//     reader.read_exact(&mut sig).unwrap();

//     if is_azcs(&mut sig) {
//         let cursor = Cursor::new(sig.to_owned());
//         let reader = azcs::decompress(cursor.chain(reader)).unwrap();
//         to_writer_internal(reader, buf, localization)
//     } else {
//         let cursor = Cursor::new(sig.to_owned());
//         let reader = cursor.chain(reader);
//         to_writer_internal(reader, buf, localization)
//     }
// }

pub enum Metadata<'a> {
    Datasheet(Datasheet<'a>),
}

// TODO: refactor this, should really be two different things
// fn to_writer_internal<'a, 'b, R, W>(
//     mut reader: R,
//     writer: &'b mut W,
//     localization: Option<&'a DashMap<String, Option<String>>>,
// ) -> io::Result<(u64, FileType, Option<Metadata<'a>>)>
// where
//     R: Read,
//     W: Write,
// {
//     let mut sig = [0; 5];
//     reader.read_exact(&mut sig)?;
//     let file_type = file_type(&sig)?;
//     let mut extra = None;

//     let size = match &file_type {
//         FileType::Luac => {
//             let buf = sig[2..5].to_owned();
//             std::io::copy(&mut buf.chain(reader), writer)
//         }
//         FileType::ObjectStream(fmt) => {
//             // early return no serialziation
//             if **fmt == ObjectStreamFormat::BYTES {
//                 return Ok((
//                     std::io::copy(&mut sig.chain(reader), writer)?,
//                     file_type,
//                     None,
//                 ));
//             };
//             let hashes = if let Some(fs) = FILESYSTEM.get() {
//                 Some(&fs.hashes)
//             } else {
//                 None
//             };

//             let Ok(obj_stream) = from_reader(&mut sig.chain(&mut reader), hashes) else {
//                 return Ok((
//                     std::io::copy(&mut sig.chain(reader), writer)?,
//                     file_type,
//                     None,
//                 ));
//             };
//             match fmt {
//                 ObjectStreamFormat::XML => {
//                     let obj_stream = XMLObjectStream::from(obj_stream);
//                     let mut buf = String::new();
//                     let mut ser = Serializer::new(&mut buf);
//                     ser.indent('\t', 2);
//                     obj_stream.serialize(ser).unwrap();
//                     std::io::copy(&mut buf.as_bytes(), writer)
//                 }
//                 ObjectStreamFormat::MINI => {
//                     let obj_stream = JSONObjectStream::from(obj_stream);
//                     let string = serde_json::to_string(&obj_stream)
//                         .expect("couldnt parse object stream to json");
//                     std::io::copy(&mut string.as_bytes(), writer)
//                 }
//                 ObjectStreamFormat::PRETTY => {
//                     let obj_stream = JSONObjectStream::from(obj_stream);
//                     let string = serde_json::to_string_pretty(&obj_stream)
//                         .expect("couldnt parse object stream to json");
//                     std::io::copy(&mut string.as_bytes(), writer)
//                 }
//                 _ => std::io::copy(&mut sig.chain(reader), writer),
//             }
//         }
//         FileType::Datasheet(fmt) => {
//             let mut reader = sig.chain(reader);
//             let datasheet = Datasheet::from(&mut reader);

//             // if **fmt == DatasheetFormat::BYTES {
//             //     return Ok((
//             //         std::io::copy(&mut sig.chain(reader), writer)?,
//             //         file_type,
//             //         Some(Metadata::Datasheet(datasheet.to_owned())),
//             //     ));
//             // };

//             extra = Some(Metadata::Datasheet(datasheet.to_owned()));

//             match fmt {
//                 DatasheetFormat::MINI => {
//                     let string = datasheet.to_json_simd(false)?;
//                     std::io::copy(&mut string.as_bytes(), writer)
//                 }
//                 DatasheetFormat::PRETTY => {
//                     let string = datasheet.to_json_simd(true)?;
//                     std::io::copy(&mut string.as_bytes(), writer)
//                 }
//                 DatasheetFormat::YAML => {
//                     let string = datasheet.to_yaml();
//                     std::io::copy(&mut string.as_bytes(), writer)
//                 }
//                 DatasheetFormat::CSV => {
//                     let string = datasheet.to_csv();
//                     std::io::copy(&mut string.as_bytes(), writer)
//                 }
//                 DatasheetFormat::BYTES => std::io::copy(&mut sig.chain(reader), writer),
//                 DatasheetFormat::XML => todo!(),
//                 DatasheetFormat::SQL => {
//                     let string = datasheet.to_sql();
//                     std::io::copy(&mut string.as_bytes(), writer)
//                 }
//             }
//         }
//         _ => std::io::copy(&mut sig.chain(reader), writer),
//     }?;

//     Ok((size, file_type, extra))
// }
