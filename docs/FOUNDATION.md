# BeautySaloon Tauri Foundation

## Eski Electron referansindan alinan kilitler

- Local SQLite ana kaynak olarak kalir; Google Calendar ve Supabase ikincil/projection katmanidir.
- Renderer dosya sistemi, SQLite, secret ve native lifecycle detaylarina dogrudan erismez.
- Musteri, personel ve hizmetlerde varsayilan kalici silme degil aktif/pasif; randevuda iptal statudur.
- Randevu kaydinda hizmet snapshot'i korunur, personel-hizmet uygunlugu ve takvim cakismasi domain katmaninda dogrulanir.
- Google Calendar tek yonlu outbox/mapping ile calisir; yalniz local mapping'i olan BeautySaloon event'leri mutate edilir.
- WhatsApp/cloud icin local reminder canonical state ve minimum cloud projection ayrimi korunur.
- Backup/restore icin SQLite integrity check, safety backup, atomic replace ve machine-bound secret sanitization gereklidir.

## Yeni mimari sinir

- Rust/Tauri: SQLite, filesystem, backup/restore, secure credential storage, tray/lifecycle ve native command boundary.
- React/TypeScript: UI, kullanici akisi, form etkilesimleri ve sunum.
- Komutlar dar ve isimli olacak; genel SQL, dosya okuma veya shell calistirma komutu verilmeyecek.

## Veri dizini

Production verisi Tauri app data klasorunde tutulacak:

- `database/salon.db`
- `backups/`
- `logs/`
- `exports/`
- `settings/`

Bu foundation smoke DB olarak `database/salon-foundation.db` kullanir; gercek V1 verisine migration veya tasima yapmaz.

## Secure storage yaklasimi

Google refresh token, Supabase session ve WhatsApp/Meta teknik secret'lari isletme DB backup'ina kor sekilde girmeyecek.
Tauri v2 icin hedef Windows Credential Manager/OS keyring uzerinden machine-bound storage kullanmak, DB icinde yalniz baglanti durumu ve guvenli mapping metadata'si tutmaktir.

## Performans yaklasimi

- WAL ve foreign keys acik.
- Buyuk listelerde pagination/limit zorunlu.
- Index'ler sorgu sekline gore migration seviyesinde eklenecek.
- Renderer ilk acilista tum musteri/randevu datasini yuklemeyecek.
- Background worker'lar yalniz acik ozellikler icin tetiklenecek.
