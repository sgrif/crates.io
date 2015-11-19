use pg::rows::Row;
use pg::GenericConnection;
use yaqb::*;

use util::{CargoResult, ChainError};
use util::errors::NotFound;

pub trait Model: Sized {
    fn from_row(row: &Row) -> Self;
    fn table_name(_: Option<Self>) -> &'static str;

    fn find(conn: &GenericConnection, id: i32) -> CargoResult<Self> {
        let sql = format!("SELECT * FROM {} WHERE id = $1",
                          Model::table_name(None::<Self>));
        let stmt = try!(conn.prepare(&sql));
        let rows = try!(stmt.query(&[&id]));
        let row = try!(rows.into_iter().next().chain_error(|| NotFound));
        Ok(Model::from_row(&row))
    }
}

type SqlTypeOfTable<T> = <<<T as ::yaqb::query_builder::UpdateTarget>::Table as Table>
                             ::Star as Expression>::SqlType;

pub fn update_or_insert<'a, T: 'a, U, V>(conn: &Connection, target: T, new_record: &'a [U;1])
    -> ::yaqb::result::Result<UpdateOrInsert<V>> where
        T: ::yaqb::query_builder::UpdateTarget + Copy,
        U: ::yaqb::query_builder::AsChangeset + ::yaqb::persistable::Insertable<'a, T::Table>
            + Copy,
        V: Queriable<SqlTypeOfTable<T>>,
        U::Changeset: ::yaqb::query_builder::Changeset<Target=T::Table>,
{
    use yaqb::query_builder::update;

    conn.transaction(|| {
        let command = update(target).set(new_record[0]);
        match try!(conn.query_one(command)) {
            Some(record) => return Ok(UpdateOrInsert::Updated(record)),
            None => {
                let record = try!(conn.insert(target.table(), new_record)).nth(0).unwrap();
                Ok(UpdateOrInsert::Inserted(record))
            }
        }
    }).map_err(|e| e.into())
}

#[derive(Debug, Clone, Copy)]
pub enum UpdateOrInsert<T> {
    Updated(T),
    Inserted(T),
}

impl<T> UpdateOrInsert<T> {
    pub fn record(self) -> T {
        match self {
            UpdateOrInsert::Updated(r) => r,
            UpdateOrInsert::Inserted(r) => r,
        }
    }
}

