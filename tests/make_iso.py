import struct, sys
def make(path, title_id, pad):
    S=0x800
    with open(path,'wb') as f:
        f.seek(32*S); f.write(b"MICROSOFT*XBOX*MEDIA"+struct.pack('<II',33,S)+b'\0'*8)
        f.seek(32*S+0x7ec); f.write(b"MICROSOFT*XBOX*MEDIA")
        d=b''
        def ent(sec,size,name):
            e=struct.pack('<HHII',0,0,sec,size)+bytes([0x80,len(name)])+name.encode()
            while len(e)%4: e+=b'\xff'
            return e
        d+=ent(34,0x200,'default.xex')
        if pad: d+=ent(40,pad,'pad.bin')
        d+=b'\xff'*4
        f.seek(33*S); f.write(d)
        x=b'XEX2'+struct.pack('>IIIII',0,0x1000,0,0,1)+struct.pack('>II',0x00040006,0x100)
        x=x.ljust(0x100,b'\0')+struct.pack('>IIII',0x12345678,1,0,title_id)+bytes([0,0,1,1])
        f.seek(34*S); f.write(x.ljust(0x200,b'\0'))
        if pad: f.seek(40*S+pad-4); f.write(b'END!')
make(sys.argv[1], int(sys.argv[2],16), int(sys.argv[3]))
