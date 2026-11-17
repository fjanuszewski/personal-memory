-- Las fuentes optativas (docs/diseno.md, 4.11): la última lectura de cada una
-- se anota por separado, para que una que no anda no demore a las demás.
-- Hasta acá había una sola fuente, WhatsApp, y su lectura quedaba como
-- «juntada» a secas.

update ajustes set clave = 'juntada:whatsapp' where clave = 'juntada';
