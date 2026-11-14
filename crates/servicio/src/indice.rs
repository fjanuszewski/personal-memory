//! Las huellas en memoria, para buscar por significado.
//!
//! Cada nota y cada pedazo de conversación tiene su huella guardada en la
//! base; al arrancar se cargan todas acá y cada búsqueda compara la huella de
//! la pregunta contra las que correspondan. Con decenas de miles de huellas
//! eso tarda milésimas de segundo y no hace falta ningún índice aproximado:
//! la respuesta es exacta.

use std::collections::HashMap;
use std::sync::RwLock;

/// Lo que se sabe de cada huella para filtrar sin ir a la base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dato {
    pub id: i64,
    /// El grupo, o `None` si no tiene.
    pub grupo: Option<i64>,
    /// Segundos desde 1970: cuándo se escribió.
    pub fecha: i64,
}

#[derive(Default)]
struct Tabla {
    /// Cuántos números tiene cada huella; 0 mientras no haya ninguna.
    largo: usize,
    datos: Vec<Dato>,
    /// Todas las huellas seguidas, cada una de largo 1 (así el parecido es un producto).
    huellas: Vec<f32>,
    lugar: HashMap<i64, usize>,
}

fn normalizada(huella: &[f32]) -> Option<Vec<f32>> {
    let norma = huella.iter().map(|x| x * x).sum::<f32>().sqrt();
    (norma.is_finite() && norma > 0.0).then(|| huella.iter().map(|x| x / norma).collect())
}

/// El producto de dos huellas del mismo largo, sumando de a ocho para que el
/// procesador lo haga en paralelo.
fn producto(a: &[f32], b: &[f32]) -> f32 {
    let mut parciales = [0.0_f32; 8];
    let (de_a_ocho_a, resto_a) = a.as_chunks::<8>();
    let (de_a_ocho_b, resto_b) = b.as_chunks::<8>();
    for (x, y) in de_a_ocho_a.iter().zip(de_a_ocho_b) {
        for i in 0..8 {
            parciales[i] += x[i] * y[i];
        }
    }
    parciales.iter().sum::<f32>() + resto_a.iter().zip(resto_b).map(|(x, y)| x * y).sum::<f32>()
}

impl Tabla {
    fn poner(&mut self, dato: Dato, huella: &[f32]) -> bool {
        let Some(huella) = normalizada(huella) else { return false };
        if self.largo == 0 {
            self.largo = huella.len();
        }
        if huella.len() != self.largo {
            return false;
        }
        if let Some(&lugar) = self.lugar.get(&dato.id) {
            self.datos[lugar] = dato;
            self.huellas[lugar * self.largo..(lugar + 1) * self.largo].copy_from_slice(&huella);
        } else {
            self.lugar.insert(dato.id, self.datos.len());
            self.datos.push(dato);
            self.huellas.extend_from_slice(&huella);
        }
        true
    }

    fn quitar(&mut self, id: i64) {
        let Some(lugar) = self.lugar.remove(&id) else { return };
        let ultimo = self.datos.len() - 1;
        if lugar != ultimo {
            // El último pasa a ocupar el hueco.
            self.datos[lugar] = self.datos[ultimo];
            self.lugar.insert(self.datos[lugar].id, lugar);
            let (antes, despues) = self.huellas.split_at_mut(ultimo * self.largo);
            antes[lugar * self.largo..(lugar + 1) * self.largo].copy_from_slice(&despues[..self.largo]);
        }
        self.datos.pop();
        self.huellas.truncate(ultimo * self.largo);
        if self.datos.is_empty() {
            self.largo = 0;
        }
    }

    fn cercanas(&self, consulta: &[f32], cuantas: usize, entra: &dyn Fn(&Dato) -> bool) -> Vec<(Dato, f64)> {
        let Some(consulta) = normalizada(consulta) else { return Vec::new() };
        if consulta.len() != self.largo || cuantas == 0 {
            return Vec::new();
        }
        let mut parecidos: Vec<(Dato, f32)> = self
            .datos
            .iter()
            .zip(self.huellas.chunks_exact(self.largo))
            .filter(|(dato, _)| entra(dato))
            .map(|(dato, huella)| (*dato, producto(&consulta, huella)))
            .collect();
        let orden = |a: &(Dato, f32), b: &(Dato, f32)| b.1.total_cmp(&a.1).then(b.0.id.cmp(&a.0.id));
        // Solo hace falta ordenar las que quedan: el resto se aparta sin ordenar.
        if parecidos.len() > cuantas {
            parecidos.select_nth_unstable_by(cuantas - 1, orden);
            parecidos.truncate(cuantas);
        }
        parecidos.sort_by(orden);
        parecidos.into_iter().map(|(dato, parecido)| (dato, f64::from(parecido))).collect()
    }
}

/// Qué huellas se guardan: las de notas y las de pedazos de conversación.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clase {
    Notas,
    Fragmentos,
}

#[derive(Default)]
pub struct Indice {
    notas: RwLock<Tabla>,
    fragmentos: RwLock<Tabla>,
}

impl Indice {
    fn tabla(&self, clase: Clase) -> &RwLock<Tabla> {
        match clase {
            Clase::Notas => &self.notas,
            Clase::Fragmentos => &self.fragmentos,
        }
    }

    /// Guarda (o reemplaza) una huella. `false` si no sirve: vacía, o de otro
    /// largo que las que ya hay.
    pub fn poner(&self, clase: Clase, dato: Dato, huella: &[f32]) -> bool {
        self.tabla(clase).write().is_ok_and(|mut t| t.poner(dato, huella))
    }

    pub fn quitar(&self, clase: Clase, id: i64) {
        if let Ok(mut t) = self.tabla(clase).write() {
            t.quitar(id);
        }
    }

    /// Cambia el grupo de las huellas que cumplan `cuales` (una conversación
    /// que pasó a otro grupo).
    pub fn mover(&self, clase: Clase, cuales: &dyn Fn(i64) -> bool, grupo: Option<i64>) {
        if let Ok(mut t) = self.tabla(clase).write() {
            for dato in t.datos.iter_mut().filter(|d| cuales(d.id)) {
                dato.grupo = grupo;
            }
        }
    }

    pub fn vaciar(&self) {
        for clase in [Clase::Notas, Clase::Fragmentos] {
            if let Ok(mut t) = self.tabla(clase).write() {
                *t = Tabla::default();
            }
        }
    }

    #[must_use]
    pub fn cuantas(&self, clase: Clase) -> usize {
        self.tabla(clase).read().map_or(0, |t| t.datos.len())
    }

    /// Las `cuantas` huellas más parecidas a `consulta` entre las que cumplen
    /// `entra`, de la más parecida a la menos, con su parecido (1 = igual).
    #[must_use]
    pub fn cercanas(
        &self,
        clase: Clase,
        consulta: &[f32],
        cuantas: usize,
        entra: &dyn Fn(&Dato) -> bool,
    ) -> Vec<(Dato, f64)> {
        self.tabla(clase).read().map_or_else(|_| Vec::new(), |t| t.cercanas(consulta, cuantas, entra))
    }
}

/// Una huella como se guarda en la base: sus números, de a cuatro bytes.
#[must_use]
pub fn a_bytes(huella: &[f32]) -> Vec<u8> {
    huella.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Lee una huella guardada. `None` si los bytes no son números enteros.
#[must_use]
pub fn de_bytes(bytes: &[u8]) -> Option<Vec<f32>> {
    let (de_a_cuatro, resto) = bytes.as_chunks::<4>();
    (resto.is_empty() && !de_a_cuatro.is_empty()).then(|| de_a_cuatro.iter().map(|b| f32::from_le_bytes(*b)).collect())
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn dato(id: i64, grupo: Option<i64>) -> Dato {
        Dato { id, grupo, fecha: id * 100 }
    }

    fn huella(direccion: usize, mezcla: f32) -> Vec<f32> {
        let mut v = vec![0.0; 16];
        v[direccion] = 1.0;
        v[(direccion + 1) % 16] = mezcla;
        v
    }

    #[test]
    fn encuentra_las_mas_parecidas_en_orden() {
        let indice = Indice::default();
        assert!(indice.poner(Clase::Notas, dato(1, Some(7)), &huella(0, 0.0)));
        assert!(indice.poner(Clase::Notas, dato(2, Some(7)), &huella(0, 0.5)));
        assert!(indice.poner(Clase::Notas, dato(3, Some(8)), &huella(5, 0.0)));
        let todas = indice.cercanas(Clase::Notas, &huella(0, 0.0), 10, &|_| true);
        assert_eq!(todas.iter().map(|(d, _)| d.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert!((todas[0].1 - 1.0).abs() < 1e-6 && todas[2].1.abs() < 1e-6, "{todas:?}");
        assert!((todas[1].1 - 1.0 / 1.25_f64.sqrt()).abs() < 1e-6, "{todas:?}");
        // El tamaño de la huella no importa, solo la dirección.
        let grande: Vec<f32> = huella(0, 0.0).iter().map(|x| x * 40.0).collect();
        assert!((indice.cercanas(Clase::Notas, &grande, 1, &|_| true)[0].1 - 1.0).abs() < 1e-6);
        // Con filtro y con tope.
        let del_grupo = indice.cercanas(Clase::Notas, &huella(5, 0.0), 10, &|d| d.grupo == Some(7));
        assert_eq!(del_grupo.iter().map(|(d, _)| d.id).collect::<Vec<_>>(), vec![2, 1]);
        assert_eq!(indice.cercanas(Clase::Notas, &huella(0, 0.0), 1, &|_| true).len(), 1);
        // Las clases no se mezclan.
        assert_eq!(indice.cercanas(Clase::Fragmentos, &huella(0, 0.0), 10, &|_| true), Vec::new());
    }

    #[test]
    fn reemplaza_quita_y_mueve() {
        let indice = Indice::default();
        for id in 1..=4 {
            indice.poner(Clase::Fragmentos, dato(id, Some(1)), &huella(usize::try_from(id).unwrap(), 0.0));
        }
        // Reemplazar una huella no la duplica.
        indice.poner(Clase::Fragmentos, dato(2, Some(1)), &huella(9, 0.0));
        assert_eq!(indice.cuantas(Clase::Fragmentos), 4);
        assert_eq!(indice.cercanas(Clase::Fragmentos, &huella(9, 0.0), 1, &|_| true)[0].0.id, 2);
        // Quitar una del medio no desacomoda a las demás.
        indice.quitar(Clase::Fragmentos, 1);
        indice.quitar(Clase::Fragmentos, 99);
        assert_eq!(indice.cuantas(Clase::Fragmentos), 3);
        for (id, direccion) in [(2, 9), (3, 3), (4, 4)] {
            assert_eq!(indice.cercanas(Clase::Fragmentos, &huella(direccion, 0.0), 1, &|_| true)[0].0.id, id);
        }
        indice.mover(Clase::Fragmentos, &|id| id == 3, Some(5));
        let del_cinco = indice.cercanas(Clase::Fragmentos, &huella(3, 0.0), 10, &|d| d.grupo == Some(5));
        assert_eq!(del_cinco.len(), 1);
        assert_eq!(del_cinco[0].0, Dato { id: 3, grupo: Some(5), fecha: 300 });
        indice.vaciar();
        assert_eq!(indice.cuantas(Clase::Fragmentos), 0);
        // Vacío, acepta huellas de otro largo.
        assert!(indice.poner(Clase::Fragmentos, dato(1, None), &[1.0, 2.0, 3.0]));
    }

    #[test]
    fn rechaza_huellas_que_no_sirven() {
        let indice = Indice::default();
        assert!(indice.poner(Clase::Notas, dato(1, None), &huella(0, 0.0)));
        assert!(!indice.poner(Clase::Notas, dato(2, None), &[1.0, 2.0]), "otro largo");
        assert!(!indice.poner(Clase::Notas, dato(3, None), &[0.0; 16]), "todo ceros");
        assert!(!indice.poner(Clase::Notas, dato(4, None), &[f32::NAN; 16]));
        assert_eq!(indice.cercanas(Clase::Notas, &[1.0, 2.0], 5, &|_| true), Vec::new());
        assert_eq!(indice.cuantas(Clase::Notas), 1);
    }

    #[test]
    fn las_huellas_se_guardan_y_se_leen_igual() {
        let original = vec![0.5_f32, -1.25, 3.0e-7];
        assert_eq!(de_bytes(&a_bytes(&original)), Some(original));
        assert_eq!(de_bytes(&[1, 2, 3]), None);
        assert_eq!(de_bytes(&[]), None);
    }
}
