FROM python:3.11-slim

WORKDIR /app

# Install git to clone the source repository
RUN apt-get update && apt-get install -y git && rm -rf /var/lib/apt/lists/*

# Clone the vmware-vsphere-mcp-server source
RUN git clone https://github.com/giuliolibrando/vmware-vsphere-mcp-server.git .

# Install Python dependencies
RUN pip install --no-cache-dir -r requirements.txt

EXPOSE 8000

CMD ["python", "main.py"]
