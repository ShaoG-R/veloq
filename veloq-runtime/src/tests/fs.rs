use crate::io::fs::File;
use crate::runtime::current_buffer_pool;
use std::fs;
use std::path::Path;

#[test]
fn test_file_integrity() {
    use crate::io::buffer::BufferSize;

    for size in [BufferSize::Size4K, BufferSize::Size16K, BufferSize::Size64K] {
        println!("Testing with BufferSize: {:?}", size);
        crate::runtime::LocalExecutor::new().block_on(async move {
            let file_path = Path::new("test_file_integrity.tmp");
            if file_path.exists() {
                fs::remove_file(file_path).unwrap();
            }

            // 1. Create and Write
            {
                let file = File::create(&file_path).await.expect("Failed to create");

                let mut write_buf = current_buffer_pool()
                    .upgrade()
                    .unwrap()
                    .alloc(size)
                    .unwrap();
                let data = b"Hello World!";
                write_buf.spare_capacity_mut()[..data.len()].copy_from_slice(data);
                write_buf.set_len(data.len());

                let (res, _) = file.write_at(write_buf, 0).await;
                assert_eq!(res.expect("Write failed"), data.len());

                file.sync_all().await.expect("Sync failed");
            }

            // 2. Open and Read
            {
                let file = File::open(&file_path).await.expect("Failed to open");

                let mut read_buf = current_buffer_pool()
                    .upgrade()
                    .unwrap()
                    .alloc(size)
                    .unwrap();
                // Ensure the buf has size to receive data, though ReadFixed usually uses capacity.
                // set_len helps if we access it later.
                read_buf.set_len(read_buf.capacity());

                let (res, read_buf) = file.read_at(read_buf, 0).await;
                let n = res.expect("Read failed");
                assert_eq!(n, 12);
                assert_eq!(&read_buf.as_slice()[..n], b"Hello World!");
            }

            // Cleanup
            fs::remove_file(file_path).unwrap();
        });
    }
}
